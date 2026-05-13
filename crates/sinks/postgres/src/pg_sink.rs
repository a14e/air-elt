use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{PgPool, QueryBuilder, Row};
use tracing::{debug, info};

use air_elt_commons_pg::Dialect;
use air_elt_commons_pg::pool;
use air_elt_commons_pg::retry::with_serialization_retry;
use air_elt_commons_pg::sink_bind::bind_value_separated;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{
    Batch, Row as CoreRow, RowOp, Schema, SchemaProvider, SinkCtx, WriteReport, WriteSpec,
};
use air_elt_core::traits::Sink;
use air_elt_core::types::{DataType, Value};

use crate::config::model::PgSinkConfig;
use crate::sql_statements as sql;

pub struct PgSinkCtx {
    /// Authoritative sink-side schema for `spec.table`. Populated once
    /// in `build_context`.
    pub schema: Schema,
    column_types: Vec<DataType>,
    insert_statement: String,
    /// `ON CONFLICT (...) DO ...` suffix derived from the flow's
    /// `[flow.<name>.conflict]` block; empty string when no conflict
    /// directive is set. Same for Postgres and CockroachDB — the standard
    /// `INSERT … ON CONFLICT` path is used in both cases.
    conflict_suffix: String,
    /// Delete plan; `Some` only when the flow declares a conflict
    /// block. The full DELETE SQL is assembled per call because the
    /// placeholder count depends on batch size (the last tick before
    /// drain may carry fewer rows than `batch_limit`); we pre-compute
    /// the prefix and the indices once.
    delete: Option<DeletePlan>,
}

struct DeletePlan {
    /// `DELETE FROM "schema"."t" WHERE (k1,..) IN (` — caller
    /// appends values and a closing `)`.
    prefix: String,
    /// Indices into `column_types` for each `conflict.key` column.
    key_indices: Vec<usize>,
}

impl SinkCtx for PgSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for PgSinkCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct PgSink {
    pool: PgPool,
    dialect: Dialect,
}

impl PgSink {
    pub async fn connect(config: PgSinkConfig) -> RuntimeResult<Self> {
        let dialect = config.dialect;
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
        Ok(Self { pool, dialect })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        sqlx::query(sql::PING)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn assert_table_writable(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        // Cheap pre-check first so the operator sees a clear privilege error
        // rather than a planner message from the probe INSERT.
        let row = sqlx::query(sql::HAS_TABLE_INSERT)
            .bind(&spec.table)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        let ok: bool = row.try_get("ok").map_err(RuntimeError::backend)?;
        if !ok {
            return Err(RuntimeError::Other(format!(
                "current user has no INSERT privilege on {:?}",
                spec.table
            )));
        }

        // Zero-row probe inside a rolled-back transaction — no sequence drift,
        // no row triggers, but the planner still validates INSERT privilege
        // and column types.
        let stmt = sql::probe_insert_where_false(&spec.table, &spec.columns)?;
        let mut tx = self.pool.begin().await.map_err(RuntimeError::backend)?;
        let exec_result = sqlx::query(&stmt).execute(&mut *tx).await;
        // Why: explicit rollback even on error — sqlx Transaction::Drop sends
        // async ROLLBACK via tokio::spawn, which may not complete if the runtime
        // is shutting down. The probe has no side effects, but explicit cleanup
        // is more predictable.
        tx.rollback().await.map_err(RuntimeError::backend)?;
        exec_result.map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Sink for PgSink {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_writable(spec).await?;
        info!(table = %spec.table, "sink access validated");
        Ok(())
    }

    async fn validate_delete_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        // Cheap privilege check first so the operator sees a clear
        // "no DELETE privilege" rather than a planner message.
        let row = sqlx::query(sql::HAS_TABLE_DELETE)
            .bind(&spec.table)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        let ok: bool = row.try_get("ok").map_err(RuntimeError::backend)?;
        if !ok {
            return Err(RuntimeError::Other(format!(
                "current user has no DELETE privilege on {:?}",
                spec.table
            )));
        }
        // Zero-row probe inside a rolled-back transaction. `WHERE false`
        // means the planner still validates DELETE syntax / privilege /
        // table visibility but no rows are touched.
        let stmt = sql::probe_delete_where_false(&spec.table)?;
        let mut tx = self.pool.begin().await.map_err(RuntimeError::backend)?;
        let exec_result = sqlx::query(&stmt).execute(&mut *tx).await;
        tx.rollback().await.map_err(RuntimeError::backend)?;
        exec_result.map_err(RuntimeError::backend)?;
        info!(table = %spec.table, "sink delete access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = air_elt_commons_pg::schema::fetch_schema(&self.pool, table).await?;
        Ok(schema)
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        let schema = self.describe_schema(&spec.table).await?;
        let column_types: Vec<DataType> = spec
            .columns
            .iter()
            .map(|c| {
                schema.find(c).map(|f| f.data_type.clone()).ok_or_else(|| {
                    RuntimeError::SchemaColumnMissing {
                        table: spec.table.clone(),
                        column: c.clone(),
                    }
                })
            })
            .collect::<RuntimeResult<_>>()?;
        let insert_statement = sql::insert_statement(&spec.table, &spec.columns)?;
        let conflict_suffix = match &spec.conflict {
            Some(c) => sql::conflict_suffix(c, &spec.columns)?,
            None => String::new(),
        };
        let delete = match &spec.conflict {
            Some(c) => Some(DeletePlan {
                prefix: sql::delete_in_prefix(&spec.table, &c.key)?,
                key_indices: c
                    .key
                    .iter()
                    .map(|k| {
                        spec.columns.iter().position(|c| c == k).ok_or_else(|| {
                            RuntimeError::Other(format!(
                                "conflict.key column {k:?} missing from mapping (loader should reject this)"
                            ))
                        })
                    })
                    .collect::<RuntimeResult<_>>()?,
            }),
            None => None,
        };
        Ok(Arc::new(PgSinkCtx {
            schema,
            column_types,
            insert_statement,
            conflict_suffix,
            delete,
        }))
    }

    async fn write_batch(
        &self,
        spec: &WriteSpec,
        ctx: &Arc<dyn SinkCtx>,
        batch: Batch,
        dry_run: bool,
    ) -> RuntimeResult<WriteReport> {
        if batch.rows.is_empty() {
            return Ok(WriteReport { rows_written: 0 });
        }
        let pg_ctx = ctx.downcast_ref_to::<PgSinkCtx>()?;
        if dry_run {
            // Dry-run path: same SQL shape as production (planner parses
            // every bind, types are checked) but the `WHERE false` /
            // `AND false` predicates short-circuit before any row is
            // touched. C20 derived rebuild semantics are unchanged —
            // `dry_run` is per-call and never affects schema lifecycle.
            self.write_upsert_batch_dry(pg_ctx, spec, &batch.rows)
                .await?;
            self.write_delete_batch_dry(pg_ctx, &batch.rows).await?;
            return Ok(WriteReport { rows_written: 0 });
        }
        // Order matters within a CDC batch: insert(id=42) followed
        // by delete(id=42) must apply upserts first; doing deletes
        // first would let the insert recreate the row we just removed.
        let upserted = self.write_upsert_batch(pg_ctx, &batch.rows).await?;
        let deleted = self.write_delete_batch(pg_ctx, &batch.rows).await?;
        Ok(WriteReport {
            rows_written: upserted + deleted,
        })
    }
}

impl PgSink {
    async fn write_upsert_batch(&self, pg_ctx: &PgSinkCtx, rows: &[CoreRow]) -> RuntimeResult<u64> {
        // The runner ships a single mixed batch; we filter once per
        // method so each helper owns a clean iterator and produces
        // its own QueryBuilder.
        if !rows.iter().any(is_upsert) {
            return Ok(0);
        }
        // The QueryBuilder is consumed by `.build().execute(...)`, so it has
        // to be rebuilt per attempt. The retry wrapper is a no-op on
        // Postgres dialect; on Cockroach it re-runs on `40001`.
        let column_types_ref = &pg_ctx.column_types;
        with_serialization_retry(self.dialect, || async {
            let mut qb: QueryBuilder<'_, sqlx::Postgres> =
                QueryBuilder::new(&pg_ctx.insert_statement);
            qb.push_values(rows.iter().filter(|r| is_upsert(r)), |mut tuple, row| {
                for (value, dt) in row.values.iter().zip(column_types_ref.iter()) {
                    bind_value_separated(&mut tuple, value, dt);
                }
            });
            if !pg_ctx.conflict_suffix.is_empty() {
                qb.push(&pg_ctx.conflict_suffix);
            }
            debug!(sql = %qb.sql(), "pg insert batch");
            let result = qb
                .build()
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(result.rows_affected())
        })
        .await
    }

    async fn write_delete_batch(&self, pg_ctx: &PgSinkCtx, rows: &[CoreRow]) -> RuntimeResult<u64> {
        if !rows.iter().any(is_delete) {
            return Ok(0);
        }
        let plan = pg_ctx.delete.as_ref().ok_or_else(|| {
            RuntimeError::Other(
                "postgres sink received Delete row but no [flow.<x>.conflict] block \
                 configured — Delete requires a key"
                    .into(),
            )
        })?;
        let key_indices = &plan.key_indices;
        let column_types_ref = &pg_ctx.column_types;
        with_serialization_retry(self.dialect, || async {
            let mut qb: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(&plan.prefix);
            if key_indices.len() == 1 {
                let mut sep = qb.separated(", ");
                let i = key_indices[0];
                let dt = &column_types_ref[i];
                for row in rows.iter().filter(|r| is_delete(r)) {
                    let v = row.values.get(i).unwrap_or(&Value::Null);
                    bind_value_separated(&mut sep, v, dt);
                }
            } else {
                qb.push_tuples(rows.iter().filter(|r| is_delete(r)), |mut tuple, row| {
                    for &i in key_indices {
                        let dt = &column_types_ref[i];
                        let v = row.values.get(i).unwrap_or(&Value::Null);
                        bind_value_separated(&mut tuple, v, dt);
                    }
                });
            }
            qb.push(")");
            debug!(sql = %qb.sql(), "pg delete batch");
            let result = qb
                .build()
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(result.rows_affected())
        })
        .await
    }
}

impl PgSink {
    async fn write_upsert_batch_dry(
        &self,
        pg_ctx: &PgSinkCtx,
        spec: &WriteSpec,
        rows: &[CoreRow],
    ) -> RuntimeResult<()> {
        if !rows.iter().any(is_upsert) {
            return Ok(());
        }
        let column_types_ref = &pg_ctx.column_types;
        let prefix = sql::dry_run_insert_prefix(&spec.table, &spec.columns)?;
        with_serialization_retry(self.dialect, || async {
            let mut qb: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(&prefix);
            qb.push_values(rows.iter().filter(|r| is_upsert(r)), |mut tuple, row| {
                for (value, dt) in row.values.iter().zip(column_types_ref.iter()) {
                    bind_value_separated(&mut tuple, value, dt);
                }
            });
            qb.push(sql::DRY_RUN_INSERT_SUFFIX);
            // Append the same ON CONFLICT clause the production builder uses,
            // so a misconfigured conflict.key (unknown column, unquoted reserved
            // word, etc.) surfaces during validate=true rather than on first real write.
            if !pg_ctx.conflict_suffix.is_empty() {
                qb.push(&pg_ctx.conflict_suffix);
            }
            debug!(sql = %qb.sql(), "pg insert batch (dry-run)");
            qb.build()
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(0u64)
        })
        .await?;
        Ok(())
    }

    async fn write_delete_batch_dry(
        &self,
        pg_ctx: &PgSinkCtx,
        rows: &[CoreRow],
    ) -> RuntimeResult<()> {
        if !rows.iter().any(is_delete) {
            return Ok(());
        }
        let plan = pg_ctx.delete.as_ref().ok_or_else(|| {
            RuntimeError::Other(
                "postgres sink received Delete row but no [flow.<x>.conflict] block \
                 configured — Delete requires a key"
                    .into(),
            )
        })?;
        let key_indices = &plan.key_indices;
        let column_types_ref = &pg_ctx.column_types;
        with_serialization_retry(self.dialect, || async {
            let mut qb: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(&plan.prefix);
            if key_indices.len() == 1 {
                let mut sep = qb.separated(", ");
                let i = key_indices[0];
                let dt = &column_types_ref[i];
                for row in rows.iter().filter(|r| is_delete(r)) {
                    let v = row.values.get(i).unwrap_or(&Value::Null);
                    bind_value_separated(&mut sep, v, dt);
                }
            } else {
                qb.push_tuples(rows.iter().filter(|r| is_delete(r)), |mut tuple, row| {
                    for &i in key_indices {
                        let dt = &column_types_ref[i];
                        let v = row.values.get(i).unwrap_or(&Value::Null);
                        bind_value_separated(&mut tuple, v, dt);
                    }
                });
            }
            qb.push(sql::DRY_RUN_DELETE_SUFFIX);
            debug!(sql = %qb.sql(), "pg delete batch (dry-run)");
            qb.build()
                .execute(&self.pool)
                .await
                .map_err(RuntimeError::backend)?;
            Ok(0u64)
        })
        .await?;
        Ok(())
    }
}

fn is_upsert(r: &CoreRow) -> bool {
    r.op == RowOp::Upsert
}

fn is_delete(r: &CoreRow) -> bool {
    r.op == RowOp::Delete
}
