use async_trait::async_trait;
use sqlx::{PgPool, Row};
use tracing::{debug, info};

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::traits::Storage;

use crate::config::model::PgStorageConfig;
use crate::sql_statements as sql;

pub struct PgStorage {
    pool: PgPool,
}

impl PgStorage {
    /// Build a `PgStorage`. The pool is opened here so `validate_access` and
    /// `migrate` can hit it immediately; timeouts and the UTC session TZ are
    /// applied by the commons pool helper.
    pub async fn connect(config: PgStorageConfig) -> RuntimeResult<Self> {
        let pool = air_elt_commons_pg::pool::connect(
            &config.url,
            air_elt_commons_pg::pool::PoolTimeouts::from_options(
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
        sqlx::migrate!("../../../migrations/storage-postgres")
            .run(&self.pool)
            .await
            .map_err(|e| RuntimeError::backend(sqlx::Error::from(e)))?;
        info!("storage migration applied");
        Ok(())
    }

    async fn load_cursor(&self, flow: &str) -> RuntimeResult<Option<CursorState>> {
        let row: Option<(serde_json::Value,)> = sqlx::query_as(sql::SELECT_CURSOR)
            .bind(flow)
            .fetch_optional(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        row.map(|(json,)| serde_json::from_value::<CursorState>(json).map_err(RuntimeError::from))
            .transpose()
    }

    async fn save_cursor(&self, flow: &str, state: &CursorState) -> RuntimeResult<()> {
        let json = serde_json::to_value(state).map_err(RuntimeError::from)?;
        sqlx::query(sql::UPSERT_CURSOR)
            .bind(flow)
            .bind(json)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}
