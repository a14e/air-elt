use std::any::Any;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgPool, QueryBuilder, Row};
use tracing::{debug, info};
use uuid::Uuid;

use air_elt_commons::sql::pg::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Batch, Schema, SinkCtx, WriteReport, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::{DataType, Value};

use crate::config::model::PgSinkConfig;
use crate::sql_statements as sql;

struct PgSinkCtx {
    column_types: Vec<DataType>,
    insert_statement: String,
}

impl SinkCtx for PgSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

pub struct PgSink {
    pool: PgPool,
}

impl PgSink {
    pub async fn connect(config: PgSinkConfig) -> RuntimeResult<Self> {
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

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = air_elt_commons::sql::pg::schema::fetch_schema(&self.pool, table).await?;
        Ok(schema)
    }

    async fn init_context(&self, spec: &WriteSpec) -> RuntimeResult<Box<dyn SinkCtx>> {
        let schema =
            air_elt_commons::sql::pg::schema::fetch_schema(&self.pool, &spec.table).await?;
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
        let insert_statement = sql::insert_statement(&spec.table, &spec.columns)?;
        Ok(Box::new(PgSinkCtx {
            column_types,
            insert_statement,
        }))
    }

    async fn write_batch(
        &self,
        _spec: &WriteSpec,
        ctx: Box<dyn SinkCtx>,
        batch: &Batch,
    ) -> RuntimeResult<(WriteReport, Box<dyn SinkCtx>)> {
        if batch.rows.is_empty() {
            return Ok((WriteReport { rows_written: 0 }, ctx));
        }

        let pg_ctx = ctx
            .as_any()
            .downcast_ref::<PgSinkCtx>()
            .ok_or_else(|| RuntimeError::Other("invalid sink context type".into()))?;

        let mut qb: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new(&pg_ctx.insert_statement);
        let column_types_ref = &pg_ctx.column_types;
        qb.push_values(batch.rows.iter(), |mut tuple, row| {
            for (value, dt) in row.values.iter().zip(column_types_ref.iter()) {
                match value {
                    Value::Null => match *dt {
                        DataType::Int64 => {
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
        debug!(rows_written, "sink batch inserted");
        Ok((WriteReport { rows_written }, ctx))
    }
}
