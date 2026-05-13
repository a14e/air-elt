use async_trait::async_trait;
use bb8::Pool;
use bb8_tiberius::ConnectionManager;
use tracing::info;

use air_elt_commons_mssql::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::traits::Storage;

use crate::config::model::MssqlStorageConfig;
use crate::sql_statements as sql;

/// Versioned SQL migrations applied in order.
///
/// Migration `0` bootstraps the ledger table itself; since the ledger
/// doesn't exist until migration 0 has run, the apply loop runs migration
/// 0 unconditionally on every `migrate()` call. The SQL is wrapped with
/// `IF OBJECT_ID IS NULL` so re-running is a no-op. Every subsequent
/// migration is recorded in the ledger and applied at most once.
const MIGRATIONS: &[(i32, &str)] = &[
    (
        0,
        include_str!("../../../../migrations/storage-mssql/0000_migrations_ledger.sql"),
    ),
    (
        1,
        include_str!("../../../../migrations/storage-mssql/0001_init.sql"),
    ),
    (
        2,
        include_str!("../../../../migrations/storage-mssql/0002_resume_tokens.sql"),
    ),
];

pub struct MssqlStorage {
    pool: Pool<ConnectionManager>,
}

impl MssqlStorage {
    pub async fn connect(config: MssqlStorageConfig) -> RuntimeResult<Self> {
        let pool = pool::connect(
            &config.url,
            pool::PoolSettings::from_options(
                config.connect_timeout,
                config.acquire_timeout,
                config.idle_timeout,
                config.max_lifetime,
                config.statement_timeout,
                config.max_connections,
                config.min_connections,
            ),
        )
        .await?;
        Ok(Self { pool })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        conn.simple_query(sql::PING)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn table_exists(&self, table: &str) -> RuntimeResult<bool> {
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        let stream = conn
            .query(sql::TABLE_EXISTS, &[&table])
            .await
            .map_err(RuntimeError::backend)?;
        let rows = stream
            .into_first_result()
            .await
            .map_err(RuntimeError::backend)?;
        if let Some(row) = rows.first() {
            let flag: Option<bool> = row.try_get::<bool, _>(0).map_err(RuntimeError::backend)?;
            Ok(flag.unwrap_or(false))
        } else {
            Ok(false)
        }
    }
}

#[async_trait]
impl Storage for MssqlStorage {
    async fn validate_access(&self) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        // Probe cursor table — if it doesn't exist yet, that's OK (not yet migrated).
        if self.table_exists(sql::CURSORS_TABLE).await? {
            let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
            conn.simple_query(sql::PROBE_INSERT_CURSORS_WHERE_FALSE)
                .await
                .map_err(RuntimeError::backend)?;
            conn.simple_query(sql::PROBE_INSERT_TOKENS_WHERE_FALSE)
                .await
                .map_err(RuntimeError::backend)?;
        }
        info!("mssql storage access validated");
        Ok(())
    }

    async fn migrate(&self) -> RuntimeResult<()> {
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;

        for (version, body) in MIGRATIONS {
            // Migration 0 bootstraps the ledger itself — it has to run
            // before we can ask the ledger whether it's been applied. The
            // SQL is guarded with IF OBJECT_ID IS NULL so re-running it
            // on subsequent migrate() calls is a no-op. For migrations >= 1
            // the ledger exists, so consult it and skip if already applied.
            if *version > 0 {
                let stream = conn
                    .query(
                        "SELECT 1 FROM _air_elt_migrations WHERE version = @P1",
                        &[version],
                    )
                    .await
                    .map_err(RuntimeError::backend)?;
                let rows = stream
                    .into_first_result()
                    .await
                    .map_err(RuntimeError::backend)?;
                if !rows.is_empty() {
                    continue;
                }
            }

            // Apply DDL and ledger upsert in a single TRY/CATCH transaction.
            // For migration 0 (ledger bootstrap) the ledger UPSERT becomes a
            // no-op if the table didn't yet exist — we MERGE so the second
            // run also records `(version = 0)` once the ledger is live.
            // If anything fails, the rollback reverts the DDL too, so a
            // retry sees a clean slate.
            let wrapped = format!(
                "BEGIN TRY \
                    BEGIN TRANSACTION; \
                    {body} \
                    IF OBJECT_ID(N'_air_elt_migrations', N'U') IS NOT NULL \
                    BEGIN \
                        MERGE _air_elt_migrations WITH (HOLDLOCK) AS target \
                        USING (VALUES (@P1)) AS source(version) \
                        ON target.version = source.version \
                        WHEN NOT MATCHED THEN INSERT (version) VALUES (source.version); \
                    END; \
                    COMMIT; \
                 END TRY \
                 BEGIN CATCH \
                    IF @@TRANCOUNT > 0 ROLLBACK; \
                    THROW; \
                 END CATCH"
            );
            conn.execute(&wrapped, &[version])
                .await
                .map_err(RuntimeError::backend)?;
            info!(version, "mssql storage migration applied");
        }
        info!("mssql storage migration complete");
        Ok(())
    }

    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>> {
        self.ensure_connection_alive().await?;
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        let stream = conn
            .query(sql::SELECT_CURSOR, &[&flow])
            .await
            .map_err(RuntimeError::backend)?;
        let rows = stream
            .into_first_result()
            .await
            .map_err(RuntimeError::backend)?;
        match rows.first() {
            Some(row) => {
                let state_str: Option<&str> =
                    row.try_get::<&str, _>(0).map_err(RuntimeError::backend)?;
                let state_str = state_str.unwrap_or("");
                if state_str.is_empty() {
                    return Ok(None);
                }
                let state: CursorState = serde_json::from_str(state_str)
                    .map_err(|e| RuntimeError::Other(format!("cursor deserialize: {e}")))?;
                Ok(Some(state))
            }
            None => Ok(None),
        }
    }

    async fn save_cursor(
        &self,
        flow: &str,
        state: &CursorState,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        let state_json = serde_json::to_string(state)
            .map_err(|e| RuntimeError::Other(format!("cursor serialize: {e}")))?;

        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;

        if dry_run {
            let wrapped = sql::dry_run_wrap(sql::UPSERT_CURSOR);
            conn.execute(&wrapped, &[&flow, &state_json.as_str()])
                .await
                .map_err(RuntimeError::backend)?;
        } else {
            conn.execute(sql::UPSERT_CURSOR, &[&flow, &state_json.as_str()])
                .await
                .map_err(RuntimeError::backend)?;
        }
        Ok(())
    }

    async fn load_resume_token(&self, flow: &str) -> RuntimeResult<Option<serde_json::Value>> {
        self.ensure_connection_alive().await?;
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        let stream = conn
            .query(sql::SELECT_RESUME_TOKEN, &[&flow])
            .await
            .map_err(RuntimeError::backend)?;
        let rows = stream
            .into_first_result()
            .await
            .map_err(RuntimeError::backend)?;
        match rows.first() {
            Some(row) => {
                let token_str: Option<&str> =
                    row.try_get::<&str, _>(0).map_err(RuntimeError::backend)?;
                let token_str = token_str.unwrap_or("");
                if token_str.is_empty() {
                    return Ok(None);
                }
                let token: serde_json::Value = serde_json::from_str(token_str)
                    .map_err(|e| RuntimeError::Other(format!("resume token deserialize: {e}")))?;
                Ok(Some(token))
            }
            None => Ok(None),
        }
    }

    async fn save_resume_token(
        &self,
        flow: &str,
        token: &serde_json::Value,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        let token_json = serde_json::to_string(token)
            .map_err(|e| RuntimeError::Other(format!("resume token serialize: {e}")))?;

        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;

        if dry_run {
            let wrapped = sql::dry_run_wrap(sql::UPSERT_RESUME_TOKEN);
            conn.execute(&wrapped, &[&flow, &token_json.as_str()])
                .await
                .map_err(RuntimeError::backend)?;
        } else {
            conn.execute(sql::UPSERT_RESUME_TOKEN, &[&flow, &token_json.as_str()])
                .await
                .map_err(RuntimeError::backend)?;
        }
        Ok(())
    }

    fn cancel_safe(&self) -> bool {
        // tiberius is not cancellation-safe; the runner spawns + detaches
        // when this returns false.
        false
    }
}
