//! QuestDB sink — pg-wire writer.
//!
//! The sink is append-only (`supports_deletes() = false`) and rejects
//! `[flow.<name>.conflict]` blocks because QuestDB's only deduplication
//! mechanism is the DDL-level `DEDUP UPSERT KEYS(...)` which is owned by
//! the user's table definition, not by the sink.
//!
//! All writes go over QuestDB's Postgres-wire surface. Each INSERT is
//! chunked to stay under QuestDB 8.2.3's `parameterCount=-2` bug at
//! ~9_300 bound parameters — see [`crate::pg_writer::QDB_PG_MAX_BIND_PARAMS`].
//!
//! ## WAL-apply visibility
//!
//! QuestDB applies WAL writes asynchronously. Even after `write_batch`
//! returns `Ok`, a subsequent pg-wire `SELECT` may not see the freshly
//! ingested rows for ~hundreds of milliseconds — read-your-write is
//! NOT guaranteed across calls. Operators querying QuestDB right after
//! a flow tick should expect a brief lag.
//! See: <https://community.questdb.com/t/how-to-await-for-rows-ingested-to-wal-table-to-become-visible-in-questdb/48>

use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use tracing::info;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_questdb::pool::{connect_pool, ping};
use air_elt_commons_questdb::schema::{SchemaWithDesignated, fetch_schema};
use air_elt_commons_questdb::types::is_questdb_native_kind;
use air_elt_core::error::{ConfigError, RuntimeError, RuntimeResult, ValidationError};
use air_elt_core::model::{Batch, Field, Schema, SchemaProvider, SinkCtx, WriteReport, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::data_type::DataType;

use crate::config::QuestDbSinkConfig;
use crate::pg_writer::PgWriter;

/// Per-flow sink context. Owns the schema introspected at `build_context`
/// time and the pg-wire writer constructed against the sink's pool.
pub struct QuestDbSinkCtx {
    pub schema: Schema,
    pub writer: PgWriter,
}

impl SinkCtx for QuestDbSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for QuestDbSinkCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct QuestDbSink {
    pool: PgPool,
    pool_max_connections: u32,
}

impl QuestDbSink {
    /// Connect to QuestDB over pg-wire.
    pub async fn connect(config: QuestDbSinkConfig) -> RuntimeResult<Self> {
        let pool_settings = PoolSettings::from_options(
            config.connect_timeout,
            None,
            config.idle_timeout,
            None,
            None,
            config.max_connections,
            config.min_connections,
        )?;
        let pool_max_connections = pool_settings.max_connections;
        let pool = connect_pool(&config.url, pool_settings).await?;
        Ok(Self {
            pool,
            pool_max_connections,
        })
    }

    /// Clone-cheap accessor used by the factory to wrap the pool in a
    /// `QuestDbPoolStatsReader`. The `PgPool` is internally `Arc`-backed.
    pub fn pool(&self) -> PgPool {
        self.pool.clone()
    }

    /// Step 1: explicit conflict-config rejection. QuestDB's only dedup
    /// hook is the DDL-level `DEDUP UPSERT KEYS(...)`, owned by the
    /// user's table — the sink cannot accept a config-level override.
    fn reject_conflict_block(spec: &WriteSpec) -> RuntimeResult<()> {
        if spec.conflict.is_some() {
            return Err(ConfigError::ConflictNotSupported {
                sink: "questdb".into(),
                hint: "QuestDB deduplication is configured via `DEDUP UPSERT KEYS(...)` \
                       in your CREATE TABLE / ALTER TABLE statement — remove the \
                       [flow.<name>.conflict] block."
                    .into(),
            }
            .into());
        }
        Ok(())
    }

    /// Step 2: introspect schema and ensure the designated timestamp
    /// column is declared on the table AND included in the write spec.
    /// Returns the schema + the designated column name.
    async fn resolve_schema_with_designated(
        &self,
        spec: &WriteSpec,
    ) -> RuntimeResult<(SchemaWithDesignated, String)> {
        let schema = fetch_schema(&self.pool, &spec.table).await?;
        let designated_column = schema.designated_column.clone().ok_or_else(|| {
            ValidationError::MissingDesignatedTimestamp {
                table: spec.table.clone(),
                column: "<none declared>".into(),
            }
        })?;
        if !spec.columns.iter().any(|c| c == &designated_column) {
            return Err(ValidationError::MissingDesignatedTimestamp {
                table: spec.table.clone(),
                column: designated_column,
            }
            .into());
        }
        Ok((schema, designated_column))
    }

    /// Step 3: single-pass column lookup + per-cell type gate. Returns
    /// the resolved `Field`s in spec order; surfaces a clear error
    /// either when a column is missing in the schema or when its type
    /// cannot be carried by the pg-wire writer.
    fn check_column_types(spec: &WriteSpec, schema: &Schema) -> RuntimeResult<Vec<Field>> {
        let mut fields: Vec<Field> = Vec::with_capacity(spec.columns.len());
        for column_name in &spec.columns {
            let field = schema
                .find(column_name)
                .ok_or_else(|| RuntimeError::SchemaColumnMissing {
                    table: spec.table.clone(),
                    column: column_name.clone(),
                })?
                .clone();
            if !type_supported(&field.data_type) {
                return Err(ValidationError::UnsupportedSinkType {
                    sink: "questdb".into(),
                    table: spec.table.clone(),
                    column: column_name.clone(),
                    type_name: format!("{}", field.data_type),
                    hint: "QuestDB supports BOOLEAN / BYTE / SHORT / INT / LONG / FLOAT / \
                           DOUBLE / STRING / VARCHAR / CHAR / DATE / TIMESTAMP / UUID / \
                           BINARY / SYMBOL / LONG256 / IPv4 / GEOHASH; other backend custom \
                           types (Mongo ObjectId, ClickHouse Enum8, ...) cannot be written."
                        .into(),
                }
                .into());
            }
            fields.push(field);
        }
        Ok(fields)
    }

    /// Step 4: probe the pg-wire writer with a never-produces-a-row
    /// statement: `INSERT INTO <table>(...) SELECT $1, $2, ..., $N WHERE 1=0`.
    /// The planner walks the column list, type-checks each bind parameter,
    /// validates table+column existence and permissions, then evaluates
    /// `WHERE 1=0` which short-circuits to zero rows. No transaction, no
    /// rollback risk, no sentinel timestamp.
    async fn dry_run_probe(&self, spec: &WriteSpec, columns: &[Field]) -> RuntimeResult<()> {
        let plan = PgWriter::build_plan(&spec.table, columns.to_vec())?;
        let writer = PgWriter::new(self.pool.clone(), plan);
        writer.dry_run().await
    }
}

#[async_trait]
impl Sink for QuestDbSink {
    fn supports_deletes(&self) -> bool {
        false
    }

    fn max_connections(&self) -> u32 {
        self.pool_max_connections
    }

    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        Self::reject_conflict_block(spec)?;
        ping(&self.pool).await?;
        let (schema_with_designated, designated_column) =
            self.resolve_schema_with_designated(spec).await?;
        let columns_in_schema = Self::check_column_types(spec, &schema_with_designated.schema)?;
        self.dry_run_probe(spec, &columns_in_schema).await?;

        info!(
            table = %spec.table,
            designated = %designated_column,
            "questdb sink access validated"
        );
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = fetch_schema(&self.pool, table).await?;
        Ok(schema.schema)
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        // The designated-timestamp + conflict-block invariants are
        // primarily enforced at `validate_access`. We re-check both
        // here so a flow with `validation.inserts = false` (which
        // skips `validate_access`) still surfaces a typed error
        // before any write hits the wire.
        Self::reject_conflict_block(spec)?;
        let schema_with_designated = fetch_schema(&self.pool, &spec.table).await?;
        if schema_with_designated.designated_column.is_none() {
            return Err(ValidationError::MissingDesignatedTimestamp {
                table: spec.table.clone(),
                column: "<none declared>".into(),
            }
            .into());
        }
        let columns: Vec<Field> = spec
            .columns
            .iter()
            .map(|c| {
                schema_with_designated
                    .schema
                    .find(c)
                    .cloned()
                    .ok_or_else(|| RuntimeError::SchemaColumnMissing {
                        table: spec.table.clone(),
                        column: c.clone(),
                    })
            })
            .collect::<RuntimeResult<_>>()?;

        let plan = PgWriter::build_plan(&spec.table, columns)?;
        let writer = PgWriter::new(self.pool.clone(), plan);
        info!(table = %spec.table, "questdb sink build_context");

        Ok(Arc::new(QuestDbSinkCtx {
            schema: schema_with_designated.schema,
            writer,
        }))
    }

    async fn write_batch(
        &self,
        _spec: &WriteSpec,
        ctx: &Arc<dyn SinkCtx>,
        batch: Batch,
        dry_run: bool,
    ) -> RuntimeResult<WriteReport> {
        if batch.rows.is_empty() {
            return Ok(WriteReport::default());
        }
        let qctx = ctx.downcast_ref_to::<QuestDbSinkCtx>()?;
        if dry_run {
            qctx.writer.dry_run().await?;
            return Ok(WriteReport::default());
        }
        let total = batch.rows.len() as u64;
        let upserts = qctx.writer.write(&batch).await?;
        // QuestDB's `pg_writer::write` drops `Delete` rows and returns
        // the count of upserts actually written. Surface the difference
        // as `skipped` so the runner can increment
        // `rows_skipped_total{op=delete}`.
        let skipped = total.saturating_sub(upserts);
        Ok(WriteReport {
            upserts,
            deletes: 0,
            skipped,
        })
    }
}

/// `true` when the canonical type can be carried by the pg-wire writer.
/// Custom types are accepted only when they are explicitly QuestDB-native
/// (`questdb.symbol`, `questdb.long256`, `questdb.geohash`). IPv4 is
/// canonical (`DataType::Ipv4`); IPv6 has no QuestDB column type.
pub fn type_supported(dt: &DataType) -> bool {
    match dt {
        DataType::Bool
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::Float32
        | DataType::Float64
        | DataType::Text { .. }
        | DataType::Bytes { .. }
        | DataType::Date
        | DataType::Timestamp
        | DataType::Uuid
        | DataType::Ipv4
        | DataType::Json => true,
        DataType::Custom(t) => is_questdb_native_kind(t.kind()),
        // BigInt/Decimal need an explicit truncate to Float64 in the
        // mapping; raw BigInt/Decimal cannot land in a QuestDB column.
        DataType::BigInt { .. } | DataType::Decimal { .. } => false,
        // No native IPv6 column in QuestDB.
        DataType::Ipv6 => false,
        DataType::Xml | DataType::Union(_) => false,
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => false,
    }
}
