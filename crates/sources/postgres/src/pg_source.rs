use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{Column, PgPool, Row};
use tracing::{debug, info};

use air_elt_commons::sql::pg::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Batch, ReadSpec, Row as CoreRow, Schema, SourceCtx};
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};

use crate::config::model::PgSourceConfig;
use crate::model::codec;
use crate::sql_statements as sql;

struct PgSourceCtx {
    schema: Arc<Schema>,
    read_query: Option<Arc<sql::ReadQuery>>,
}

impl SourceCtx for PgSourceCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

pub struct PgSource {
    pool: PgPool,
}

impl PgSource {
    pub async fn connect(config: PgSourceConfig) -> RuntimeResult<Self> {
        let pool = pool::connect(
            &config.url,
            pool::PoolTimeouts::from_options(
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
    async fn validate_access(&self, spec: &ReadSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_readable(spec).await?;
        info!(table = %spec.table, "source access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = air_elt_commons::sql::pg::schema::fetch_schema(&self.pool, table).await?;
        Ok(schema)
    }

    async fn init_context(&self, spec: &ReadSpec) -> RuntimeResult<Box<dyn SourceCtx>> {
        let schema = Arc::new(
            air_elt_commons::sql::pg::schema::fetch_schema(&self.pool, &spec.table).await?,
        );
        Ok(Box::new(PgSourceCtx {
            schema,
            read_query: None,
        }))
    }

    async fn read_batch<'a>(
        &self,
        spec: &ReadSpec,
        ctx: Box<dyn SourceCtx>,
        cursor: Option<&'a CursorState>,
    ) -> RuntimeResult<(Batch, Box<dyn SourceCtx>)> {
        let mut pg_ctx = ctx
            .into_any()
            .downcast::<PgSourceCtx>()
            .map_err(|_| RuntimeError::Other("invalid source context type".into()))?;

        let column_types = resolve_types(&pg_ctx.schema, &spec.columns, &spec.table)?;
        let cursor_types = resolve_types(&pg_ctx.schema, &spec.cursor_fields, &spec.table)?;
        let cursor_nullable: Vec<bool> = spec
            .cursor_fields
            .iter()
            .map(|name| {
                pg_ctx
                    .schema
                    .find(name)
                    .map(|f| f.nullable)
                    .unwrap_or(false)
            })
            .collect();

        // Cache read_query for non-null cursors; rebuild when any cursor value
        // is NULL because the SQL predicate changes per null-pattern.
        let all_non_null = cursor
            .map(|c| c.fields.iter().all(|f| !f.value.is_null()))
            .unwrap_or(false);

        let query_plan = if all_non_null {
            if let Some(cached) = &pg_ctx.read_query {
                cached.clone()
            } else {
                let plan = Arc::new(sql::build_read_batch(
                    &spec.table,
                    &spec.columns,
                    &spec.cursor_fields,
                    spec.cursor_order,
                    cursor,
                    &cursor_nullable,
                )?);
                pg_ctx.read_query = Some(plan.clone());
                plan
            }
        } else {
            Arc::new(sql::build_read_batch(
                &spec.table,
                &spec.columns,
                &spec.cursor_fields,
                spec.cursor_order,
                cursor,
                &cursor_nullable,
            )?)
        };

        debug!(sql = %query_plan.sql, "read_batch sql");

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
                let dt = cursor_types[cursor_field_pos];
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
            for (idx, dt) in column_types.iter().enumerate() {
                values.push(codec::decode_column(row, idx, *dt)?);
            }

            let mut cursor_values = Vec::with_capacity(spec.cursor_fields.len());
            for (cf_idx, cursor_field) in spec.cursor_fields.iter().enumerate() {
                let dt = cursor_types[cf_idx];
                let value = match spec.columns.iter().position(|c| c == cursor_field) {
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
            out_rows.push(CoreRow { values });
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

        Ok((
            Batch {
                rows: out_rows,
                next_cursor,
            },
            pg_ctx as Box<dyn SourceCtx>,
        ))
    }
}

fn resolve_types(schema: &Schema, names: &[String], table: &str) -> RuntimeResult<Vec<DataType>> {
    names
        .iter()
        .map(|name| {
            schema.find(name).map(|f| f.data_type).ok_or_else(|| {
                RuntimeError::Other(format!(
                    "column {name:?} not in source schema for {table:?}"
                ))
            })
        })
        .collect()
}
