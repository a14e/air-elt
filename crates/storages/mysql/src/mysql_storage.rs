use async_trait::async_trait;
use sqlx::{MySqlPool, Row};
use tracing::{debug, info};

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::traits::Storage;

use crate::config::model::MySqlStorageConfig;
use crate::sql_statements as sql;

pub struct MySqlStorage {
    pool: MySqlPool,
    /// Pre-resolved at connect time so `save_cursor` does not pay the cost
    /// per call. Picks the row-alias form on MySQL ≥ 8.0.19 (avoids the
    /// 8.0.20 deprecation warning) or the legacy `VALUES(col)` form on
    /// MariaDB / older MySQL.
    upsert_sql: &'static str,
}

impl MySqlStorage {
    pub async fn connect(config: MySqlStorageConfig) -> RuntimeResult<Self> {
        let pool = air_elt_commons_mysql::pool::connect(
            &config.url,
            air_elt_commons_mysql::pool::PoolSettings::from_options(
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
        let version: String = sqlx::query_scalar("SELECT VERSION()")
            .fetch_one(&pool)
            .await
            .map_err(RuntimeError::backend)?;
        let upsert_sql = sql::pick_upsert_cursor(&version);
        debug!(version = %version, "mysql storage connected");
        Ok(Self { pool, upsert_sql })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        sqlx::query(sql::PING)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn cursor_table_exists(&self) -> RuntimeResult<bool> {
        let row = sqlx::query(sql::TABLE_EXISTS)
            .bind(sql::CURSORS_TABLE)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        let exists: i64 = row
            .try_get::<i64, _>("exists_flag")
            .map_err(RuntimeError::backend)?;
        Ok(exists != 0)
    }

    async fn assert_cursor_table_writable(&self) -> RuntimeResult<()> {
        // MySQL has no `has_table_privilege()` analogue. Probe-insert inside
        // a rolled-back transaction validates INSERT privilege + column-type
        // alignment without side effects.
        let mut tx = self.pool.begin().await.map_err(RuntimeError::backend)?;
        let exec_result = sqlx::query(sql::PROBE_INSERT_WHERE_FALSE)
            .execute(&mut *tx)
            .await;
        tx.rollback().await.map_err(RuntimeError::backend)?;
        exec_result.map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Storage for MySqlStorage {
    async fn validate_access(&self) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        let exists = self.cursor_table_exists().await?;
        debug!(exists, "mysql storage cursor table existence");
        if exists {
            self.assert_cursor_table_writable().await?;
        }
        // If the table doesn't exist, `migrate` will create it. We don't
        // pre-check CREATE privilege — MySQL doesn't expose a clean
        // `has_schema_privilege` analogue and the migration error itself is
        // descriptive.
        Ok(())
    }

    async fn migrate(&self) -> RuntimeResult<()> {
        sqlx::migrate!("../../../migrations/storage-mysql")
            .run(&self.pool)
            .await
            .map_err(|e| RuntimeError::backend(sqlx::Error::from(e)))?;
        info!("mysql storage migration applied");
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
        sqlx::query(self.upsert_sql)
            .bind(flow)
            .bind(json)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}
