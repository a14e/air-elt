//! pg-wire writer with bind-param chunking.
//!
//! QuestDB 8.2.3 pg-wire returns `invalid parameter count [parameterCount=-2]`
//! at roughly 9,300 bound parameters per statement — well below Postgres'
//! `u16::MAX` limit. We cap each statement at [`QDB_PG_MAX_BIND_PARAMS`]
//! bound params; `rows_per_chunk = QDB_PG_MAX_BIND_PARAMS / columns.len()`.
//!
//! QuestDB pg-wire auto-commits each statement server-side, so the
//! production write path issues each chunk directly against the pool.
//! The dry-run path emits a single
//! `INSERT INTO <t> (...) SELECT $1,...,$N WHERE 1=0` statement — the
//! planner walks the column list, type-checks each bind, and the
//! `WHERE 1=0` short-circuits to zero rows. No transaction, no rollback
//! risk against QuestDB's async WAL apply.

use chrono::{NaiveDate, Utc};
use sqlx::{PgPool, Postgres, QueryBuilder};
use tracing::debug;
use uuid::Uuid;

use air_elt_commons_questdb::pg_bind::bind_value_separated_pg;
use air_elt_commons_questdb::types::geohash::QuestDbGeohashValue;
use air_elt_commons_questdb::types::long256::QuestDbLong256Value;
use air_elt_commons_questdb::types::symbol::QuestDbSymbolValue;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Batch, Field, Row, RowOp};
use air_elt_core::types::Value;
use air_elt_core::types::data_type::DataType;

use crate::sql_statements::{dry_run_sql_pg, insert_sql_pg};

/// Empirical safe limit for QuestDB 8.2.3 pg-wire bound-param counts.
/// 9_200 keeps us under the `parameterCount=-2` overflow observed at
/// ~9_300 — see the bench notes in the AIR-38 plan.
pub const QDB_PG_MAX_BIND_PARAMS: usize = 9_200;

/// Immutable per-flow plan for the pg-wire writer.
#[derive(Debug, Clone)]
pub struct PgWritePlan {
    /// Sink table name (qualified, unquoted). Kept so the dry-run probe
    /// can re-render the statement via `dry_run_sql_pg`.
    pub table: String,
    /// Pre-formed `INSERT INTO "<table>" (...) ` prefix.
    pub insert_prefix: String,
    /// Sink columns in declaration order. Drives the per-cell bind dispatch.
    pub columns: Vec<Field>,
}

/// pg-wire writer for the QuestDB sink. Bundles the connection pool with
/// the per-flow plan; constructed once at `build_context` time and held
/// on `QuestDbSinkCtx` for the lifetime of the flow.
#[derive(Debug, Clone)]
pub struct PgWriter {
    pool: PgPool,
    plan: PgWritePlan,
}

impl PgWriter {
    /// Build a plan from a `(table, columns)` pair. Returns the plan
    /// suitable for use as input to [`PgWriter::new`].
    pub fn build_plan(table: &str, columns: Vec<Field>) -> RuntimeResult<PgWritePlan> {
        let column_names: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
        let insert_prefix = insert_sql_pg(table, &column_names)?;
        Ok(PgWritePlan {
            table: table.to_string(),
            insert_prefix,
            columns,
        })
    }

    /// Construct a writer from an already-open pool and a pre-built plan.
    pub fn new(pool: PgPool, plan: PgWritePlan) -> Self {
        Self { pool, plan }
    }

    /// Write every `RowOp::Upsert` row in `batch` against the pool. Each
    /// chunk is issued directly — QuestDB pg-wire auto-commits server-side.
    /// Returns the total number of rows committed.
    pub async fn write(&self, batch: &Batch) -> RuntimeResult<u64> {
        let upserts: Vec<&Row> = batch
            .rows
            .iter()
            .filter(|r| r.op == RowOp::Upsert)
            .collect();
        if upserts.is_empty() || self.plan.columns.is_empty() {
            return Ok(0);
        }

        let rows_per_chunk = (QDB_PG_MAX_BIND_PARAMS / self.plan.columns.len()).max(1);
        let mut rows_written: u64 = 0;
        for chunk in upserts.chunks(rows_per_chunk) {
            let mut qb = build_chunk_query(&self.plan, chunk)?;
            debug!(
                rows = chunk.len(),
                bind_params = chunk.len() * self.plan.columns.len(),
                "questdb pg-wire insert chunk"
            );
            qb.build()
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            rows_written += chunk.len() as u64;
        }
        Ok(rows_written)
    }

    /// Probe the planner + bind path with a single never-produces-a-row
    /// statement. Emits
    /// `INSERT INTO <t> (...) SELECT $1, $2, ..., $N WHERE 1=0` — the
    /// planner walks the column list, type-checks each bind parameter,
    /// validates table+column existence and permissions, then evaluates
    /// `WHERE 1=0` which short-circuits to zero rows. No transaction, no
    /// rollback risk against QuestDB's async WAL apply.
    pub async fn dry_run(&self) -> RuntimeResult<()> {
        if self.plan.columns.is_empty() {
            return Ok(());
        }
        let column_names: Vec<String> = self.plan.columns.iter().map(|c| c.name.clone()).collect();
        // Full statement shape:
        // `INSERT INTO "<t>" (...) SELECT $1,...,$N FROM long_sequence(0)`.
        // sqlx's `Separated::push_bind` auto-numbers `$N` for us, so we
        // produce the canonical SQL via the helper, split off the
        // placeholder section, fill it via sqlx, then re-append the
        // `FROM long_sequence(0)` tail.
        let sql = dry_run_sql_pg(&self.plan.table, &column_names)?;
        let head = sql
            .split_once("SELECT ")
            .map(|(left, _)| left)
            .unwrap_or(&sql);
        let mut qb: QueryBuilder<'_, Postgres> = QueryBuilder::new(head);
        qb.push("SELECT ");
        {
            let mut sep = qb.separated(", ");
            for field in &self.plan.columns {
                let dummy = dry_run_dummy_value(field);
                bind_value_separated_pg(&mut sep, field, &dummy).map_err(RuntimeError::from)?;
            }
        }
        qb.push(" FROM long_sequence(0)");
        debug!(
            columns = self.plan.columns.len(),
            "questdb pg-wire dry-run probe"
        );
        qb.build()
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}

fn build_chunk_query<'q>(
    plan: &'q PgWritePlan,
    chunk: &[&Row],
) -> RuntimeResult<QueryBuilder<'q, Postgres>> {
    let mut qb: QueryBuilder<'q, Postgres> = QueryBuilder::new(&plan.insert_prefix);
    bind_chunk(&mut qb, &plan.columns, chunk)?;
    Ok(qb)
}

fn bind_chunk(
    qb: &mut QueryBuilder<'_, Postgres>,
    columns: &[Field],
    chunk: &[&Row],
) -> RuntimeResult<()> {
    let mut row_bind_error: Option<RuntimeError> = None;
    qb.push_values(chunk.iter(), |mut row, src| {
        if row_bind_error.is_some() {
            return;
        }
        for (i, field) in columns.iter().enumerate() {
            let v = src.values.get(i).unwrap_or(&Value::Null);
            if let Err(error) = bind_value_separated_pg(&mut row, field, v) {
                row_bind_error = Some(error.into());
                return;
            }
        }
    });
    match row_bind_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// Pick a safe non-NULL placeholder value matching the field's canonical
/// type. Used by the dry-run probe to exercise the planner's type-check on
/// every column — NULL bypasses pg's bind-time type inference so we send
/// concrete values instead.
fn dry_run_dummy_value(field: &Field) -> Value {
    match &field.data_type {
        DataType::Bool => Value::Bool(false),
        DataType::Int8 => Value::Int8(0),
        DataType::Int16 => Value::Int16(0),
        DataType::Int32 => Value::Int32(0),
        DataType::Int64 => Value::Int64(0),
        DataType::Float32 => Value::Float32(0.0),
        DataType::Float64 => Value::Float64(0.0),
        DataType::Text { .. } => Value::Text(String::new()),
        DataType::Bytes { .. } => Value::Bytes(Vec::new()),
        DataType::Date => {
            Value::Date(NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch date constant"))
        }
        DataType::Timestamp => Value::Timestamp(Utc::now()),
        DataType::Uuid => Value::Uuid(Uuid::nil()),
        DataType::Ipv4 => Value::Ipv4(std::net::Ipv4Addr::LOCALHOST),
        DataType::Json => Value::Json(serde_json::Value::Null),
        DataType::Custom(t) => dummy_custom_value(t.as_ref()),
        // Unsupported types reach here only as Null because `type_supported`
        // already rejected them at validate_access; emit Null to keep the
        // probe binding safe.
        _ => Value::Null,
    }
}

fn dummy_custom_value(t: &dyn air_elt_core::types::dynamic::DynType) -> Value {
    use air_elt_commons_questdb::types::geohash::QuestDbGeohashType;
    use air_elt_commons_questdb::types::long256::QuestDbLong256Type;
    use air_elt_commons_questdb::types::symbol::QuestDbSymbolType;

    let kind = t.kind();
    if kind == QuestDbSymbolType::KIND {
        return Value::Custom(Box::new(QuestDbSymbolValue(String::new())));
    }
    if kind == QuestDbLong256Type::KIND {
        return Value::Custom(Box::new(QuestDbLong256Value([0u8; 32])));
    }
    if let Some(g) = t.as_any().downcast_ref::<QuestDbGeohashType>() {
        return Value::Custom(Box::new(QuestDbGeohashValue {
            bits: g.bits,
            value: 0,
        }));
    }
    // Unknown custom kinds reach here only if `type_supported` was bypassed;
    // fall back to Null so the probe surfaces a typed error rather than
    // panicking.
    Value::Null
}
