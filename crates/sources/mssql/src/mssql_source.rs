use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use bb8::Pool;
use bb8_tiberius::ConnectionManager;
use tiberius::ToSql;
use tracing::{debug, info};

use air_elt_commons_mssql::pool;
use air_elt_commons_mssql::value_bind::{BoundValue, value_to_column_data};
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::raw::{RawBatch, RawRow};
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::model::{ReadSpec, Schema, SchemaProvider, SourceCtx};
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};

use crate::config::model::MssqlSourceConfig;
use crate::model::codec;
use crate::sql_statements as sql;

pub struct MssqlSourceCtx {
    pub schema: Schema,
    column_types: Vec<DataType>,
    cursor_types: Vec<DataType>,
    cursor_nullable: Vec<bool>,
    cursor_to_column: Vec<Option<usize>>,
    /// Pre-built first-tick query (no WHERE clause, no params).
    initial_read_query: sql::ReadQuery,
}

impl SourceCtx for MssqlSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for MssqlSourceCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct MssqlSource {
    pool: Pool<ConnectionManager>,
    name: String,
}

impl MssqlSource {
    pub async fn connect(name: String, config: MssqlSourceConfig) -> RuntimeResult<Self> {
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
        Ok(Self { pool, name })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        conn.simple_query(sql::PING)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn assert_table_readable(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        let stmt = sql::probe_select(&spec.table, &spec.columns)?;
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        conn.simple_query(&stmt)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Source for MssqlSource {
    fn name(&self) -> &str {
        &self.name
    }

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_readable(spec).await?;
        info!(table = %spec.table, "mssql source access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        air_elt_commons_mssql::schema::fetch_schema(&self.pool, table).await
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
        let initial_read_query = sql::build_read_batch(
            &spec.table,
            &spec.columns,
            &spec.cursor_fields,
            spec.cursor_order,
            None,
            &cursor_nullable,
            &cursor_types,
            spec.limit,
        )?;
        Ok(Arc::new(MssqlSourceCtx {
            schema,
            column_types,
            cursor_types,
            cursor_nullable,
            cursor_to_column,
            initial_read_query,
        }))
    }

    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<RawBatch> {
        let my_ctx = ctx.downcast_ref_to::<MssqlSourceCtx>()?;

        // First tick: reuse the cached zero-parameter query.
        let (sql_text, params, param_types) = match cursor {
            None => (
                my_ctx.initial_read_query.sql.clone(),
                my_ctx.initial_read_query.params.clone(),
                my_ctx.initial_read_query.param_types.clone(),
            ),
            Some(state) => {
                let q = sql::build_read_batch(
                    &spec.table,
                    &spec.columns,
                    &spec.cursor_fields,
                    spec.cursor_order,
                    Some(state),
                    &my_ctx.cursor_nullable,
                    &my_ctx.cursor_types,
                    spec.limit,
                )?;
                (q.sql, q.params, q.param_types)
            }
        };

        debug!(sql = %sql_text, "mssql read_batch sql");

        // Bind every parameter through value_bind. NULL is typed via the
        // declared DataType so tiberius sends the right TDS variant.
        let bound: Vec<BoundValue> = params
            .iter()
            .zip(param_types.iter())
            .map(|(v, t)| value_to_column_data(v, t).map(BoundValue))
            .collect::<RuntimeResult<Vec<_>>>()?;
        let refs: Vec<&dyn ToSql> = bound.iter().map(|b| b as &dyn ToSql).collect();

        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        let stream = conn
            .query(&sql_text, &refs)
            .await
            .map_err(RuntimeError::backend)?;

        let rows = stream
            .into_first_result()
            .await
            .map_err(RuntimeError::backend)?;

        let mut out_rows = Vec::with_capacity(rows.len());
        let mut last_cursor_values: Option<Vec<Value>> = None;
        for row in &rows {
            let mut values = Vec::with_capacity(spec.columns.len());
            for (idx, dt) in my_ctx.column_types.iter().enumerate() {
                values.push(codec::decode_column(row, idx, dt)?);
            }

            let mut cursor_values = Vec::with_capacity(spec.cursor_fields.len());
            for (cf_idx, cursor_field) in spec.cursor_fields.iter().enumerate() {
                let dt = &my_ctx.cursor_types[cf_idx];
                let value = match my_ctx.cursor_to_column[cf_idx] {
                    Some(i) => values[i].clone(),
                    None => {
                        let pos = row
                            .columns()
                            .iter()
                            .position(|c| c.name() == cursor_field.as_str())
                            .ok_or_else(|| {
                                RuntimeError::Other(format!(
                                    "cursor field {cursor_field:?} not present in SELECT"
                                ))
                            })?;
                        codec::decode_column(row, pos, dt)?
                    }
                };
                cursor_values.push(value);
            }
            last_cursor_values = Some(cursor_values);
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

    fn cancel_safe(&self) -> bool {
        // tiberius is not cancellation-safe; the runner spawns + detaches
        // when this returns false.
        false
    }
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
