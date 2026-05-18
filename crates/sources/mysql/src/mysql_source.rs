use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{Column, MySqlPool, Row as SqlxRow};
use tracing::{debug, info};

use air_elt_commons_mysql::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Batch, Row};
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::model::{ReadSpec, Schema, SchemaProvider, SourceCtx};
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};

use crate::config::model::MySqlSourceConfig;
use crate::model::codec;
use crate::sql_statements as sql;

pub struct MySqlSourceCtx {
    /// Authoritative source-side schema for `spec.table`. Populated
    /// once in `build_context`.
    pub schema: Schema,
    initial_read_query: Arc<sql::ReadQuery>,
    non_null_read_query: Arc<sql::ReadQuery>,
    column_types: Vec<DataType>,
    cursor_types: Vec<DataType>,
    cursor_nullable: Vec<bool>,
    cursor_to_column: Vec<Option<usize>>,
}

impl SourceCtx for MySqlSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for MySqlSourceCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct MySqlSource {
    pool: MySqlPool,
    name: String,
    pool_max_connections: u32,
}

impl MySqlSource {
    pub async fn connect(name: String, config: MySqlSourceConfig) -> RuntimeResult<Self> {
        let pool_settings = pool::PoolSettings::from_options(
            config.connect_timeout,
            config.acquire_timeout,
            config.idle_timeout,
            config.max_lifetime,
            config.statement_timeout,
            config.max_connections,
            config.min_connections,
        )?;
        let pool_max_connections = pool_settings.max_connections;
        let pool = pool::connect(&config.url, pool_settings).await?;
        Ok(Self {
            pool,
            name,
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

    /// MySQL has no `has_table_privilege()` analogue we can pre-check
    /// cleanly. The probe SELECT below fails fast if SELECT is denied — the
    /// resulting error already carries the privilege complaint from the
    /// server. Skipping the pre-check matches the pattern used elsewhere in
    /// this crate.
    async fn assert_table_readable(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        let stmt = sql::probe_select(&spec.table, &spec.columns)?;
        sqlx::query(&stmt)
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Source for MySqlSource {
    fn name(&self) -> &str {
        &self.name
    }

    fn max_connections(&self) -> u32 {
        self.pool_max_connections
    }

    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_readable(spec).await?;
        info!(table = %spec.table, "mysql source access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = air_elt_commons_mysql::schema::fetch_schema(&self.pool, table).await?;
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
        let initial_read_query = Arc::new(sql::build_read_batch(
            &spec.table,
            &spec.columns,
            &spec.cursor_fields,
            spec.cursor_order,
            None,
            &cursor_nullable,
        )?);
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
            &spec.cursor_fields,
            spec.cursor_order,
            Some(&sentinel_state),
            &cursor_nullable,
        )?);
        Ok(Arc::new(MySqlSourceCtx {
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
        ctx: &Arc<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<Batch> {
        let my_ctx = ctx.downcast_ref_to::<MySqlSourceCtx>()?;

        let query_plan = match cursor {
            None => my_ctx.initial_read_query.clone(),
            Some(c) if c.fields.iter().all(|f| !f.value.is_null()) => {
                my_ctx.non_null_read_query.clone()
            }
            Some(_) => Arc::new(sql::build_read_batch(
                &spec.table,
                &spec.columns,
                &spec.cursor_fields,
                spec.cursor_order,
                cursor,
                &my_ctx.cursor_nullable,
            )?),
        };

        debug!(sql = %query_plan.sql, "mysql read_batch sql");

        let mut query = sqlx::query(&query_plan.sql);
        if let Some(state) = cursor {
            for idx in &query_plan.bind_order {
                let field = state
                    .fields
                    .get(*idx)
                    .ok_or_else(|| RuntimeError::Other("cursor bind index out of range".into()))?;
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
                let dt = my_ctx.cursor_types[cursor_field_pos].clone();
                query = codec::bind_cursor_value(query, &field.value, dt);
            }
        }
        query = query.bind(i64::try_from(spec.limit).map_err(|_| {
            RuntimeError::Other(format!("batch_limit {} does not fit in i64", spec.limit))
        })?);

        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;

        let mut out_rows = Vec::with_capacity(rows.len());
        let mut last_cursor_values: Option<Vec<Value>> = None;
        for row in &rows {
            let mut values = Vec::with_capacity(spec.columns.len());
            for (idx, dt) in my_ctx.column_types.iter().enumerate() {
                values.push(codec::decode_column(row, idx, dt.clone())?);
            }

            let mut cursor_values = Vec::with_capacity(spec.cursor_fields.len());
            for (cf_idx, cursor_field) in spec.cursor_fields.iter().enumerate() {
                let dt = my_ctx.cursor_types[cf_idx].clone();
                let value = match my_ctx.cursor_to_column[cf_idx] {
                    Some(i) => values[i].clone(),
                    None => {
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
            let body = if spec.needs_body {
                let json = air_elt_core::transform::build_body_json(&values, &spec.columns)?;
                Some(Value::Json(json))
            } else {
                None
            };
            out_rows.push(Row::upsert(values).with_body(body));
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

        Ok(Batch {
            rows: out_rows,
            next_cursor,
        })
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
