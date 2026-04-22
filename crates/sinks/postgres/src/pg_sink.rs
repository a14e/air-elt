use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, QueryBuilder, Row};
use tokio::sync::Mutex;
use tracing::{debug, info};
use uuid::Uuid;

use air_elt_commons::sql::pg::identifier::split_qualified;
use air_elt_commons::sql::pg::pg_type::{self, PgType};
use air_elt_commons::sql::pg::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::schema::{Field, Schema};
use air_elt_core::traits::{Batch, Sink, WriteReport, WriteSpec};
use air_elt_core::types::{DataType, Value};

use crate::config::model::PgSinkConfig;
use crate::sql_statements as sql;

pub struct PgSink {
    pool: PgPool,
    schema_cache: Mutex<HashMap<String, Arc<Schema>>>,
}

impl PgSink {
    pub async fn connect(config: PgSinkConfig) -> RuntimeResult<Self> {
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
        sqlx::query(&stmt)
            .execute(&mut *tx)
            .await
            .map_err(RuntimeError::backend)?;
        tx.rollback().await.map_err(RuntimeError::backend)?;
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
impl Sink for PgSink {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_writable(spec).await?;
        info!(table = %spec.table, "sink access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = self.cached_schema(table).await?;
        Ok((*schema).clone())
    }

    async fn write_batch(&self, spec: &WriteSpec, batch: &Batch) -> RuntimeResult<WriteReport> {
        if batch.rows.is_empty() {
            return Ok(WriteReport { rows_written: 0 });
        }

        let schema = self.cached_schema(&spec.table).await?;
        let column_types: Vec<DataType> = spec
            .columns
            .iter()
            .map(|c| {
                schema.find(c).map(|f| f.data_type).ok_or_else(|| {
                    RuntimeError::Other(format!(
                        "column {c:?} missing in sink schema for {:?}",
                        spec.table
                    ))
                })
            })
            .collect::<RuntimeResult<_>>()?;
        let prefix = sql::insert_prefix(&spec.table, &spec.columns)?;
        let mut qb: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(prefix);
        let column_types_ref = &column_types;
        qb.push_values(batch.rows.iter(), |mut tuple, row| {
            for (value, dt) in row.values.iter().zip(column_types_ref.iter()) {
                match value {
                    Value::Null => match *dt {
                        DataType::Null | DataType::Int64 => {
                            tuple.push_bind::<Option<i64>>(None);
                        }
                        DataType::Bool => {
                            tuple.push_bind::<Option<bool>>(None);
                        }
                        DataType::Int16 => {
                            tuple.push_bind::<Option<i16>>(None);
                        }
                        DataType::Int32 => {
                            tuple.push_bind::<Option<i32>>(None);
                        }
                        DataType::Float32 => {
                            tuple.push_bind::<Option<f32>>(None);
                        }
                        DataType::Float64 => {
                            tuple.push_bind::<Option<f64>>(None);
                        }
                        DataType::Text => {
                            tuple.push_bind::<Option<String>>(None);
                        }
                        DataType::Bytes => {
                            tuple.push_bind::<Option<Vec<u8>>>(None);
                        }
                        DataType::Date => {
                            tuple.push_bind::<Option<NaiveDate>>(None);
                        }
                        DataType::Timestamp => {
                            tuple.push_bind::<Option<DateTime<Utc>>>(None);
                        }
                        DataType::Uuid => {
                            tuple.push_bind::<Option<Uuid>>(None);
                        }
                        DataType::Json => {
                            tuple.push_bind::<Option<serde_json::Value>>(None);
                        }
                    },
                    Value::Bool(b) => {
                        tuple.push_bind(*b);
                    }
                    Value::Int16(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::Int32(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::Int64(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::Float32(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::Float64(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::Text(s) => {
                        tuple.push_bind(s.as_str());
                    }
                    Value::Bytes(b) => {
                        tuple.push_bind(b.as_slice());
                    }
                    Value::Date(d) => {
                        tuple.push_bind(*d);
                    }
                    Value::Timestamp(ts) => {
                        tuple.push_bind(*ts);
                    }
                    Value::Uuid(u) => {
                        tuple.push_bind(*u);
                    }
                    Value::Json(j) => {
                        tuple.push_bind(j);
                    }
                }
            }
        });
        debug!(sql = %qb.sql(), rows = batch.rows.len(), "insert batch sql");
        let result = qb
            .build()
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;

        let rows_written = result.rows_affected();
        info!(rows_written, "sink batch inserted");
        Ok(WriteReport { rows_written })
    }
}
