use async_trait::async_trait;
use sqlx::{PgPool, Row};
use tracing::{debug, info};

use air_elt_commons_pg::Dialect;
use air_elt_commons_pg::retry::with_serialization_retry;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::traits::Storage;
use air_elt_core::types::DataType;

use crate::config::model::PgStorageConfig;
use crate::sql_statements as sql;

pub struct PgStorage {
    pool: PgPool,
    dialect: Dialect,
    pool_max_connections: u32,
}

impl PgStorage {
    /// Build a `PgStorage`. The pool is opened here so `validate_access` and
    /// `migrate` can hit it immediately; timeouts and the UTC session TZ are
    /// applied by the commons pool helper.
    pub async fn connect(config: PgStorageConfig) -> RuntimeResult<Self> {
        let dialect = config.dialect;
        let pool_settings = air_elt_commons_pg::pool::PoolSettings::from_options(
            config.connect_timeout,
            config.acquire_timeout,
            config.idle_timeout,
            config.max_lifetime,
            config.statement_timeout,
            config.max_connections,
            config.min_connections,
        )?;
        let pool_max_connections = pool_settings.max_connections;
        let pool = air_elt_commons_pg::pool::connect(&config.url, pool_settings).await?;
        Ok(Self {
            pool,
            dialect,
            pool_max_connections,
        })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        sqlx::query(sql::PING)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn cursor_table_exists(&self) -> RuntimeResult<bool> {
        sqlx::query_scalar(sql::TABLE_EXISTS)
            .bind(sql::CURSORS_TABLE)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)
    }

    async fn assert_schema_create_privilege(&self) -> RuntimeResult<()> {
        let row = sqlx::query(sql::HAS_CREATE_PRIVILEGE)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        let ok: bool = row.try_get("ok").map_err(RuntimeError::backend)?;
        if !ok {
            return Err(RuntimeError::Other(
                "current user has no CREATE privilege on the current schema".to_string(),
            ));
        }
        Ok(())
    }

    async fn assert_cursor_table_insert_privilege(&self) -> RuntimeResult<()> {
        let row = sqlx::query(sql::HAS_TABLE_INSERT)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        let ok: bool = row.try_get("ok").map_err(RuntimeError::backend)?;
        if !ok {
            return Err(RuntimeError::Other(
                "current user has no INSERT privilege on air_elt_cursors".to_string(),
            ));
        }
        Ok(())
    }

    async fn assert_cursor_table_writable(&self) -> RuntimeResult<()> {
        let mut tx = self.pool.begin().await.map_err(RuntimeError::backend)?;
        // Why: zero-row INSERT inside a rolled-back tx validates INSERT
        // privilege + column-type alignment without tripping sequences or
        // row-level triggers.
        // Store exec result first so rollback is always called even on error.
        let exec_result = sqlx::query(sql::PROBE_INSERT_WHERE_FALSE)
            .execute(&mut *tx)
            .await;
        // Why: explicit rollback even on error — sqlx Transaction::Drop sends
        // async ROLLBACK via tokio::spawn, which may not complete if the runtime
        // is shutting down. Explicit cleanup is more predictable.
        tx.rollback().await.map_err(RuntimeError::backend)?;
        exec_result.map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Storage for PgStorage {
    fn max_connections(&self) -> u32 {
        self.pool_max_connections
    }

    async fn validate_access(&self) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        let exists = self.cursor_table_exists().await?;
        debug!(exists, "storage cursor table existence");
        if exists {
            self.assert_cursor_table_insert_privilege().await?;
            self.assert_cursor_table_writable().await?;
        } else {
            self.assert_schema_create_privilege().await?;
        }
        Ok(())
    }

    async fn migrate(&self) -> RuntimeResult<()> {
        // Why: `sqlx::migrate!` resolves its path at compile time, so each
        // dialect needs its own literal. Both directories must exist before
        // this compiles. The Cockroach migrations are byte-for-byte copies
        // of the Postgres ones today (TEXT/JSONB/TIMESTAMPTZ/now() are all
        // supported); they're kept separate so future divergence has a
        // home.
        match self.dialect {
            Dialect::Postgres => {
                sqlx::migrate!("../../../migrations/storage-postgres")
                    .run(&self.pool)
                    .await
                    .map_err(|e| RuntimeError::backend(sqlx::Error::from(e)))?;
            }
            Dialect::Cockroach => {
                // CockroachDB doesn't implement `pg_advisory_lock()`, which
                // sqlx's migrator uses by default to coordinate concurrent
                // migrators. Disable the locking step here — single-node
                // migrations are sequential anyway, and a running cluster
                // is expected to roll the schema once at deploy time.
                let mut migrator = sqlx::migrate!("../../../migrations/storage-cockroachdb");
                migrator.set_locking(false);
                migrator
                    .run(&self.pool)
                    .await
                    .map_err(|e| RuntimeError::backend(sqlx::Error::from(e)))?;
            }
        }
        info!("storage migration applied");
        Ok(())
    }

    async fn load_cursor(
        &self,
        flow: &str,
        cursor_types: &[DataType],
    ) -> RuntimeResult<Option<CursorState>> {
        with_serialization_retry(self.dialect, || async {
            let row: Option<(serde_json::Value,)> = sqlx::query_as(sql::SELECT_CURSOR)
                .bind(flow)
                .fetch_optional(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            row.map(|(json,)| CursorState::from_typed_json(json, cursor_types))
                .transpose()
        })
        .await
    }

    async fn save_cursor(
        &self,
        flow: &str,
        state: &CursorState,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        // Always serialize: a serialization failure is a real bug we
        // want to surface even in dry-run. Only the network execute
        // is skipped.
        let json = serde_json::to_value(state).map_err(RuntimeError::from)?;
        if dry_run {
            return Ok(());
        }
        with_serialization_retry(self.dialect, || async {
            sqlx::query(sql::UPSERT_CURSOR)
                .bind(flow)
                .bind(json.clone())
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(())
        })
        .await
    }

    async fn load_resume_token(&self, flow: &str) -> RuntimeResult<Option<serde_json::Value>> {
        with_serialization_retry(self.dialect, || async {
            let row: Option<(serde_json::Value,)> = sqlx::query_as(sql::SELECT_RESUME_TOKEN)
                .bind(flow)
                .fetch_optional(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(row.map(|(j,)| j))
        })
        .await
    }

    async fn save_resume_token(
        &self,
        flow: &str,
        token: &serde_json::Value,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        // Always serialize: a serialization failure is a real bug we
        // want to surface even in dry-run. Only the network execute
        // is skipped.
        let json = serde_json::to_value(token).map_err(RuntimeError::from)?;
        if dry_run {
            return Ok(());
        }
        with_serialization_retry(self.dialect, || async {
            sqlx::query(sql::UPSERT_RESUME_TOKEN)
                .bind(flow)
                .bind(json.clone())
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(())
        })
        .await
    }
}
