use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tracing::{debug, info};

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_clickhouse::client::{ChClient, ChClientConfig, ChCompression};
use air_elt_commons_clickhouse::row_binary::encode_value;
use air_elt_commons_clickhouse::schema::fetch_schema;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{
    Batch, Field, RowOp, Schema, SchemaProvider, SinkCtx, WriteReport, WriteSpec,
};
use air_elt_core::traits::Sink;

use crate::config::model::ChSinkConfig;
use crate::sql_statements as sql;

/// Per-flow sink context. Caches the column shape (post-mapping
/// expansion) and the INSERT SQL that will be sent with each batch.
pub struct ChSinkCtx {
    pub schema: Schema,
    /// `Field`s for each mapped sink column in `WriteSpec.columns`
    /// order. Used by the RowBinary encoder.
    columns: Vec<Field>,
    insert_sql: String,
}

impl SinkCtx for ChSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for ChSinkCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct ChSink {
    client: ChClient,
}

impl ChSink {
    pub async fn connect(config: ChSinkConfig) -> RuntimeResult<Self> {
        let defaults = PoolSettings::defaults();
        let pool = PoolSettings::from_options(
            config.connect_timeout,
            config.acquire_timeout,
            config.idle_timeout,
            Some(defaults.max_lifetime),
            config.request_timeout,
            config.max_connections,
            Some(defaults.min_connections),
        );
        // Map a vanishingly small per-stmt timeout to a usable default,
        // matching the SQL backends.
        let pool = PoolSettings {
            statement: pool.statement.max(Duration::from_secs(1)),
            ..pool
        };
        let compression = match config.compression {
            crate::config::model::ChCompressionKind::None => ChCompression::None,
            crate::config::model::ChCompressionKind::Lz4 => ChCompression::Lz4,
        };
        let client = ChClient::connect(ChClientConfig {
            url: config.url,
            database: config.database,
            user: config.user,
            password: config.password,
            pool,
            compression,
        })
        .map_err(RuntimeError::backend)?;
        Ok(Self { client })
    }
}

#[async_trait]
impl Sink for ChSink {
    fn supports_deletes(&self) -> bool {
        false
    }

    fn cancel_safe(&self) -> bool {
        // `reqwest` futures are cancel-safe — the underlying connection
        // is managed by the HTTP pool, not the dropped future. Default
        // would also be `true`; we override explicitly to document the
        // posture (matches the sqlx-backed sinks).
        true
    }

    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        // Liveness probe.
        self.client.ping().await.map_err(RuntimeError::backend)?;
        // Type/permission probe: zero-row INSERT.
        let probe = sql::probe_insert_where_false(&spec.table, &spec.columns)?;
        self.client
            .query_text(&probe)
            .await
            .map_err(RuntimeError::backend)?;
        info!(table = %spec.table, "clickhouse sink access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        fetch_schema(&self.client, table)
            .await
            .map_err(RuntimeError::backend)
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        let schema = self.describe_schema(&spec.table).await?;
        let mut columns: Vec<Field> = Vec::with_capacity(spec.columns.len());
        for c in &spec.columns {
            let f = schema
                .find(c)
                .ok_or_else(|| RuntimeError::SchemaColumnMissing {
                    table: spec.table.clone(),
                    column: c.clone(),
                })?;
            columns.push(f.clone());
        }
        let insert_sql = sql::insert_row_binary_sql(&spec.table, &spec.columns)?;
        Ok(Arc::new(ChSinkCtx {
            schema,
            columns,
            insert_sql,
        }))
    }

    async fn write_batch(
        &self,
        _spec: &WriteSpec,
        ctx: Arc<dyn SinkCtx>,
        batch: Batch,
        dry_run: bool,
    ) -> RuntimeResult<WriteReport> {
        if batch.rows.is_empty() {
            return Ok(WriteReport { rows_written: 0 });
        }
        let ch_ctx = ctx.downcast_ref_to::<ChSinkCtx>()?;
        if dry_run {
            // Use EXPLAIN INSERT to validate column names, types, and
            // the RowBinary format without actually inserting.
            let explain_sql = format!("EXPLAIN {}", ch_ctx.insert_sql);
            self.client
                .query_text(&explain_sql)
                .await
                .map_err(RuntimeError::backend)?;
            return Ok(WriteReport { rows_written: 0 });
        }
        // Runner pre-filters Delete rows when `supports_deletes() = false`.
        // Defensive check: skip Deletes if they ever reach the sink.
        let mut body: Vec<u8> = Vec::with_capacity(batch.rows.len() * ch_ctx.columns.len() * 8);
        let mut rows_written: u64 = 0;
        for row in batch.rows.iter().filter(|r| r.op == RowOp::Upsert) {
            rows_written += 1;
            for (i, field) in ch_ctx.columns.iter().enumerate() {
                let v = row
                    .values
                    .get(i)
                    .unwrap_or(&air_elt_core::types::Value::Null);
                encode_value(&mut body, field, v).map_err(RuntimeError::backend)?;
            }
        }
        if rows_written == 0 {
            return Ok(WriteReport { rows_written: 0 });
        }
        debug!(
            rows = rows_written,
            bytes = body.len(),
            "clickhouse row-binary insert"
        );
        self.client
            .insert_row_binary(&ch_ctx.insert_sql, body)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(WriteReport { rows_written })
    }
}
