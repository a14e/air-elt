use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{Column, PgPool, Row};
use tokio::sync::Mutex;
use tracing::{debug, info};

use air_elt_commons::sql::pg::identifier::split_qualified;
use air_elt_commons::sql::pg::null_bind;
use air_elt_commons::sql::pg::pg_type::{self, PgType};
use air_elt_commons::sql::pg::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::flow::state::{CursorFieldValue, CursorState};
use air_elt_core::schema::{Field, Schema};
use air_elt_core::traits::{Batch, ReadSpec, Row as CoreRow, Source};
use air_elt_core::types::{DataType, Value};

use crate::config::model::PgSourceConfig;
use crate::model::mapping as value_codec;
use crate::sql_statements as sql;

pub struct PgSource {
    pool: PgPool,
    // Why: validation's `describe_schema` is the only introspection round-trip
    // we need per flow lifetime — the schema does not change under a running
    // daemon. Caching it here means `read_batch` never hits information_schema
    // on the hot path. Runtime schema drift is not detected (fails loud at
    // first sqlx binding mismatch), tracked for a later iteration.
    schema_cache: Mutex<HashMap<String, Arc<Schema>>>,
}

impl PgSource {
    pub async fn connect(config: PgSourceConfig) -> RuntimeResult<Self> {
        let pool = pool::connect(
            &config.url,
            pool::PoolTimeouts::from_options(
                config.connect_timeout_secs,
                config.acquire_timeout_secs,
                config.idle_timeout_secs,
                config.max_lifetime_secs,
                config.statement_timeout_secs,
            ),
        )
        .await?;
        Ok(Self {
            pool,
            schema_cache: Mutex::new(HashMap::new()),
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
        // Symmetric to sink: cheap has_table_privilege pre-check, then the
        // zero-row SELECT actually validates column-level SELECT privilege
        // and existence.
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

    async fn cached_schema(&self, table: &str) -> RuntimeResult<Arc<Schema>> {
        let mut cache = self.schema_cache.lock().await;
        if let Some(p) = cache.get(table) {
            return Ok(p.clone());
        }
        let schema = Arc::new(self.fetch_schema(table).await?);
        cache.insert(table.to_string(), schema.clone());
        Ok(schema)
    }

    async fn fetch_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let (schema_name, table_name) = split_qualified(table);
        let rows: Vec<(String, String, String, String)> = sqlx::query_as(sql::INFORMATION_SCHEMA)
            .bind(&schema_name)
            .bind(&table_name)
            .fetch_all(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;

        if rows.is_empty() {
            return Err(RuntimeError::Other(format!(
                "table {schema_name:?}.{table_name:?} not found or not visible to current user"
            )));
        }

        let mut fields = Vec::with_capacity(rows.len());
        for (col, is_null, udt, data_type) in rows {
            let pg: PgType = PgType::parse(&udt)
                .or_else(|| PgType::parse(&data_type))
                .ok_or_else(|| {
                    RuntimeError::Other(format!(
                        "unsupported pg type for column {col:?}: udt={udt:?}, data_type={data_type:?}"
                    ))
                })?;
            fields.push(Field {
                name: col,
                data_type: pg_type::to_internal(pg),
                nullable: is_null.eq_ignore_ascii_case("YES"),
            });
        }
        Ok(Schema::new(fields))
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
        // Warms the cache so subsequent read_batch calls are lookup-only.
        let schema = self.cached_schema(table).await?;
        Ok((*schema).clone())
    }

    async fn read_batch(
        &self,
        spec: &ReadSpec,
        cursor: Option<&CursorState>,
    ) -> RuntimeResult<Batch> {
        let schema = self.cached_schema(&spec.table).await?;
        let column_types = resolve_types(&schema, &spec.columns, &spec.table)?;
        let cursor_types = resolve_types(&schema, &spec.cursor_fields, &spec.table)?;

        let query_plan = sql::build_read_batch(
            &spec.table,
            &spec.columns,
            &spec.cursor_fields,
            spec.cursor_order,
            cursor,
        )?;
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
                query = bind_or_typed_null(query, &field.value, dt);
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
                values.push(value_codec::decode_column(row, idx, *dt)?);
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
                        value_codec::decode_column(row, idx, dt)?
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
            schema.find(name).map(|f| f.data_type).ok_or_else(|| {
                RuntimeError::Other(format!(
                    "column {name:?} not in source schema for {table:?}"
                ))
            })
        })
        .collect()
}

fn bind_or_typed_null<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: &'q Value,
    dt: DataType,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        Value::Null => null_bind::bind_typed_null(query, dt),
        _ => value_codec::bind_cursor_value(query, value),
    }
}
