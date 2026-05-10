use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{Column, PgPool, Row};
use tracing::{debug, info};

use air_elt_commons_pg::Dialect;
use air_elt_commons_pg::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::raw::{RawBatch, RawRow};
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::model::{ReadSpec, Schema, SchemaProvider, SourceCtx};
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};

use crate::config::model::PgSourceConfig;
use crate::model::codec;
use crate::sql_statements as sql;

pub struct PgSourceCtx {
    /// Authoritative source-side schema for `spec.table`. Computed once
    /// in `build_context` and reused by validation.
    /// Refresh = drop the ctx Arc; the runner does this on
    /// `RuntimeError::Backend`.
    pub schema: Schema,
    /// Pre-built plan for the initial tick (no cursor → no WHERE).
    initial_read_query: Arc<sql::ReadQuery>,
    /// Pre-built plan for the all-non-null cursor path. Cursor values are
    /// bound per-call; only the SQL shape is cached. The null-cursor path
    /// rebuilds per call because its predicate shape varies per null
    /// pattern.
    non_null_read_query: Arc<sql::ReadQuery>,
    column_types: Vec<DataType>,
    cursor_types: Vec<DataType>,
    cursor_nullable: Vec<bool>,
    /// Pre-computed cursor → column index lookup, hoisted out of the row
    /// decode loop.
    cursor_to_column: Vec<Option<usize>>,
}

impl SourceCtx for PgSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for PgSourceCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct PgSource {
    pool: PgPool,
    name: String,
    dialect: Dialect,
}

impl PgSource {
    pub async fn connect(name: String, config: PgSourceConfig) -> RuntimeResult<Self> {
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
        Ok(Self {
            pool,
            name,
            dialect,
        })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        sqlx::query(sql::PING)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn assert_table_readable(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        let row = sqlx::query(sql::HAS_TABLE_SELECT)
            .bind(&spec.table)
            .fetch_one(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        let ok: bool = row.try_get("ok").map_err(RuntimeError::backend)?;
        if !ok {
            return Err(RuntimeError::Other(format!(
                "current user has no SELECT privilege on {:?}",
                spec.table
            )));
        }

        let stmt = sql::probe_select(&spec.table, &spec.columns)?;
        sqlx::query(&stmt)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Source for PgSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_readable(spec).await?;
        let schema = air_elt_commons_pg::schema::fetch_schema(&self.pool, &spec.table).await?;
        reject_excluded_types(self.dialect, &schema, &spec.columns)?;
        info!(table = %spec.table, "source access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = air_elt_commons_pg::schema::fetch_schema(&self.pool, table).await?;
        Ok(schema)
    }

    async fn build_context(&self, spec: &ReadSpec) -> RuntimeResult<Arc<dyn SourceCtx>> {
        let schema = self.describe_schema(&spec.table).await?;
        let column_types = resolve_types(&schema, &spec.columns, &spec.table)?;
        let cursor_types = resolve_types(&schema, &spec.cursor_fields, &spec.table)?;
        let cursor_nullable: Vec<bool> = spec
            .cursor_fields
            .iter()
            .map(|name| schema.find(name).map(|f| f.nullable).unwrap_or(false))
            .collect();
        let cursor_to_column: Vec<Option<usize>> = spec
            .cursor_fields
            .iter()
            .map(|cf| spec.columns.iter().position(|c| c == cf))
            .collect();
        // Pre-build two static plans up front. Cursor values are bound at
        // call time; only SQL shape is cached. The null-aware path stays
        // dynamic because its predicate shape varies per null pattern.
        let initial_read_query = Arc::new(sql::build_read_batch(
            &spec.table,
            &spec.columns,
            &column_types,
            &spec.cursor_fields,
            spec.cursor_order,
            None,
            &cursor_nullable,
        )?);
        // Build a sentinel cursor state with non-null placeholder values to
        // stamp out the SQL shape for the all-non-null path. The placeholder
        // type doesn't matter — bind_order is positional and real values are
        // bound at read time.
        let sentinel_fields: Vec<CursorFieldValue> = spec
            .cursor_fields
            .iter()
            .map(|name| CursorFieldValue {
                name: name.clone(),
                value: Value::Int64(0),
            })
            .collect();
        let sentinel_state = CursorState::new(sentinel_fields);
        let non_null_read_query = Arc::new(sql::build_read_batch(
            &spec.table,
            &spec.columns,
            &column_types,
            &spec.cursor_fields,
            spec.cursor_order,
            Some(&sentinel_state),
            &cursor_nullable,
        )?);
        Ok(Arc::new(PgSourceCtx {
            schema,
            initial_read_query,
            non_null_read_query,
            column_types,
            cursor_types,
            cursor_nullable,
            cursor_to_column,
        }))
    }

    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<RawBatch> {
        let pg_ctx = ctx.downcast_ref_to::<PgSourceCtx>()?;

        let query_plan = match cursor {
            None => pg_ctx.initial_read_query.clone(),
            Some(c) if c.fields.iter().all(|f| !f.value.is_null()) => {
                pg_ctx.non_null_read_query.clone()
            }
            Some(_) => Arc::new(sql::build_read_batch(
                &spec.table,
                &spec.columns,
                &pg_ctx.column_types,
                &spec.cursor_fields,
                spec.cursor_order,
                cursor,
                &pg_ctx.cursor_nullable,
            )?),
        };

        debug!(sql = %query_plan.sql, "read_batch sql");

        let limit = i64::try_from(spec.limit).map_err(|_| {
            RuntimeError::Other(format!("batch_limit {} does not fit in i64", spec.limit))
        })?;

        // Bind + execute is wrapped in `with_serialization_retry`. On the
        // Postgres dialect the retry helper is a pure pass-through (single
        // execution), so behaviour is unchanged. On Cockroach a `40001`
        // serialization failure under contention is retried with backoff.
        // The closure rebuilds the `sqlx::query` each attempt because
        // `fetch_all` consumes it.
        let rows = air_elt_commons_pg::retry::with_serialization_retry(self.dialect, || async {
            let mut query = sqlx::query(&query_plan.sql);
            if let Some(state) = cursor {
                for idx in &query_plan.bind_order {
                    let field = state.fields.get(*idx).ok_or_else(|| {
                        RuntimeError::Other("cursor bind index out of range".into())
                    })?;
                    let cursor_field_pos = spec
                        .cursor_fields
                        .iter()
                        .position(|n| n == &field.name)
                        .ok_or_else(|| {
                            RuntimeError::Other(format!(
                                "cursor field {:?} not present in spec.cursor_fields",
                                field.name
                            ))
                        })?;
                    let dt = pg_ctx.cursor_types[cursor_field_pos].clone();
                    query = codec::bind_cursor_value(query, &field.value, dt);
                }
            }
            query = query.bind(limit);
            query
                .fetch_all(&self.pool)
                .await
                .map_err(RuntimeError::backend)
        })
        .await?;

        let mut out_rows = Vec::with_capacity(rows.len());
        let mut last_cursor_values: Option<Vec<Value>> = None;
        for row in &rows {
            let mut values = Vec::with_capacity(spec.columns.len());
            for (idx, dt) in pg_ctx.column_types.iter().enumerate() {
                values.push(codec::decode_column(row, idx, dt.clone())?);
            }

            let mut cursor_values = Vec::with_capacity(spec.cursor_fields.len());
            for (cf_idx, cursor_field) in spec.cursor_fields.iter().enumerate() {
                let dt = pg_ctx.cursor_types[cf_idx].clone();
                let value = match pg_ctx.cursor_to_column[cf_idx] {
                    Some(i) => values[i].clone(),
                    None => {
                        // Cursor fields are guaranteed by validation to also
                        // appear in mapping.from (and thus in spec.columns).
                        // This branch is defence-in-depth; if tripped, the
                        // validation pipeline has a bug.
                        let idx = row
                            .columns()
                            .iter()
                            .position(|c| c.name() == cursor_field.as_str())
                            .ok_or_else(|| {
                                RuntimeError::Other(format!(
                                    "cursor field {cursor_field:?} not present in SELECT"
                                ))
                            })?;
                        codec::decode_column(row, idx, dt)?
                    }
                };
                cursor_values.push(value);
            }
            last_cursor_values = Some(cursor_values);
            // Cost-guarded body: when the flow has body targets the
            // Transform interpreter `Body` op consumes the attached
            // `Value::Json(...)`. For non-body flows we skip the
            // encoding entirely (no extra allocation).
            let body = if spec.needs_body {
                let json = air_elt_core::transform::build_body_json(&values, &spec.columns)?;
                Some(Value::Json(json))
            } else {
                None
            };
            out_rows.push(RawRow::upsert(values).with_body(body));
        }

        let next_cursor = last_cursor_values.map(|values| {
            let fields = spec
                .cursor_fields
                .iter()
                .zip(values)
                .map(|(name, value)| CursorFieldValue {
                    name: name.clone(),
                    value,
                })
                .collect();
            CursorState::new(fields)
        });

        Ok(RawBatch {
            rows: out_rows,
            next_cursor,
        })
    }
}

/// Reject columns whose declared type the dialect refuses to serve.
///
/// Currently used for CockroachDB, which has no `XML` type. Centralised in a
/// free function so it can be unit-tested without spinning up a database;
/// `validate_access` is the only production caller.
fn reject_excluded_types(
    dialect: Dialect,
    schema: &Schema,
    columns: &[String],
) -> RuntimeResult<()> {
    for col in columns {
        let Some(field) = schema.find(col) else {
            // Missing-column reporting is the matrix's job, not ours.
            continue;
        };
        if let Some(reason) = dialect.excludes_type(&field.data_type) {
            return Err(RuntimeError::Other(format!(
                "source column {col:?} has type {dt:?} unsupported on this dialect: {reason}",
                dt = field.data_type
            )));
        }
    }
    Ok(())
}

fn resolve_types(schema: &Schema, names: &[String], table: &str) -> RuntimeResult<Vec<DataType>> {
    names
        .iter()
        .map(|name| {
            schema
                .find(name)
                .map(|f| f.data_type.clone())
                .ok_or_else(|| {
                    RuntimeError::Other(format!(
                        "column {name:?} not in source schema for {table:?}"
                    ))
                })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_core::model::Field;

    fn schema_with_xml() -> Schema {
        Schema::new(vec![
            Field {
                name: "id".into(),
                data_type: DataType::Int64,
                nullable: false,
            },
            Field {
                name: "doc".into(),
                data_type: DataType::Xml,
                nullable: true,
            },
        ])
    }

    #[test]
    fn cockroach_rejects_xml_column() {
        let schema = schema_with_xml();
        let err = reject_excluded_types(
            Dialect::Cockroach,
            &schema,
            &["id".to_string(), "doc".to_string()],
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("doc"), "msg: {msg}");
        assert!(msg.contains("Xml"), "msg: {msg}");
        assert!(msg.contains("CockroachDB has no XML type"), "msg: {msg}");
    }

    #[test]
    fn cockroach_accepts_non_xml_columns() {
        let schema = schema_with_xml();
        reject_excluded_types(Dialect::Cockroach, &schema, &["id".to_string()]).unwrap();
    }

    #[test]
    fn postgres_accepts_xml_column() {
        let schema = schema_with_xml();
        reject_excluded_types(
            Dialect::Postgres,
            &schema,
            &["id".to_string(), "doc".to_string()],
        )
        .unwrap();
    }

    #[test]
    fn missing_column_is_silently_skipped_here() {
        // Reporting "column not in schema" is the matrix's job — this helper
        // only enforces the dialect type allow-list.
        let schema = schema_with_xml();
        reject_excluded_types(Dialect::Cockroach, &schema, &["nonexistent".to_string()]).unwrap();
    }
}
