use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{MySqlPool, QueryBuilder};
use tracing::{debug, info};

use air_elt_commons_mysql::pool;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Batch, Schema, SinkCtx, WriteReport, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::{DataType, Value};

use crate::config::model::MySqlSinkConfig;
use crate::sql_statements as sql;

struct MySqlSinkCtx {
    column_types: Vec<DataType>,
    insert_statement: String,
}

impl SinkCtx for MySqlSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

pub struct MySqlSink {
    pool: MySqlPool,
}

impl MySqlSink {
    pub async fn connect(config: MySqlSinkConfig) -> RuntimeResult<Self> {
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
        // MySQL has no `has_table_privilege()` analogue; rely on the probe
        // INSERT below to surface privilege errors. Wrap in a transaction
        // we always roll back so the planner-validated zero-row insert
        // leaves no side effects.
        let stmt = sql::probe_insert_where_false(&spec.table, &spec.columns)?;
        let mut tx = self.pool.begin().await.map_err(RuntimeError::backend)?;
        let exec_result = sqlx::query(&stmt).execute(&mut *tx).await;
        tx.rollback().await.map_err(RuntimeError::backend)?;
        exec_result.map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Sink for MySqlSink {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        self.assert_table_writable(spec).await?;
        info!(table = %spec.table, "mysql sink access validated");
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        let schema = air_elt_commons_mysql::schema::fetch_schema(&self.pool, table).await?;
        Ok(schema)
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        let schema = air_elt_commons_mysql::schema::fetch_schema(&self.pool, &spec.table).await?;
        let column_types: Vec<DataType> = spec
            .columns
            .iter()
            .map(|c| {
                schema.find(c).map(|f| f.data_type).ok_or_else(|| {
                    RuntimeError::SchemaColumnMissing {
                        table: spec.table.clone(),
                        column: c.clone(),
                    }
                })
            })
            .collect::<RuntimeResult<_>>()?;
        let insert_statement = sql::insert_statement(&spec.table, &spec.columns)?;
        Ok(Arc::new(MySqlSinkCtx {
            column_types,
            insert_statement,
        }))
    }

    async fn write_batch(
        &self,
        _spec: &WriteSpec,
        ctx: Arc<dyn SinkCtx>,
        batch: &Batch,
    ) -> RuntimeResult<WriteReport> {
        if batch.rows.is_empty() {
            return Ok(WriteReport { rows_written: 0 });
        }

        let my_ctx = ctx.downcast_ref_to::<MySqlSinkCtx>()?;

        let mut qb: QueryBuilder<'_, sqlx::MySql> = QueryBuilder::new(&my_ctx.insert_statement);
        let column_types_ref = &my_ctx.column_types;
        qb.push_values(batch.rows.iter(), |mut tuple, row| {
            for (value, dt) in row.values.iter().zip(column_types_ref.iter()) {
                match value {
                    // Keep in sync with air_elt_commons_mysql::null_bind::bind_typed_null
                    // — the Separated lifetime forces inlining here.
                    Value::Null => match *dt {
                        DataType::Bool => {
                            tuple.push_bind::<Option<bool>>(None);
                        }
                        DataType::Int16 => {
                            tuple.push_bind::<Option<i16>>(None);
                        }
                        DataType::Int32 => {
                            tuple.push_bind::<Option<i32>>(None);
                        }
                        DataType::Int64 => {
                            tuple.push_bind::<Option<i64>>(None);
                        }
                        DataType::Float32 => {
                            tuple.push_bind::<Option<f32>>(None);
                        }
                        DataType::Float64 => {
                            tuple.push_bind::<Option<f64>>(None);
                        }
                        DataType::Text { .. } => {
                            tuple.push_bind::<Option<String>>(None);
                        }
                        DataType::Bytes { .. } => {
                            tuple.push_bind::<Option<Vec<u8>>>(None);
                        }
                        DataType::Date => {
                            tuple.push_bind::<Option<NaiveDate>>(None);
                        }
                        DataType::Timestamp => {
                            tuple.push_bind::<Option<DateTime<Utc>>>(None);
                        }
                        DataType::Uuid => {
                            // Match the non-null UUID path which binds as
                            // canonical text (see Value::Uuid arm below).
                            tuple.push_bind::<Option<String>>(None);
                        }
                        DataType::Json => {
                            tuple.push_bind::<Option<serde_json::Value>>(None);
                        }
                        DataType::BigInt { .. } | DataType::Decimal { .. } => {
                            tuple.push_bind::<Option<bigdecimal::BigDecimal>>(None);
                        }
                        DataType::UInt8 => {
                            tuple.push_bind::<Option<u8>>(None);
                        }
                        DataType::UInt16 => {
                            tuple.push_bind::<Option<u16>>(None);
                        }
                        DataType::UInt32 => {
                            tuple.push_bind::<Option<u32>>(None);
                        }
                        DataType::UInt64 => {
                            tuple.push_bind::<Option<u64>>(None);
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
                        // MariaDB's native UUID column applies an internal
                        // byte-shuffle to indexable v1 timestamps when the
                        // input is binary; binding as canonical text bypasses
                        // that and round-trips correctly. Stock MySQL has no
                        // UUID type, so this branch only fires when the sink
                        // schema declared `DataType::Uuid` (i.e. MariaDB).
                        tuple.push_bind(u.to_string());
                    }
                    Value::Json(j) => {
                        tuple.push_bind(j);
                    }
                    Value::BigInt(b) => {
                        // sqlx-mysql encodes `decimal` only via `BigDecimal`;
                        // wrap the bigint with scale 0.
                        tuple.push_bind(bigdecimal::BigDecimal::new(b.clone(), 0));
                    }
                    Value::Decimal(d) => {
                        tuple.push_bind(d.clone());
                    }
                    Value::UInt8(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::UInt16(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::UInt32(n) => {
                        tuple.push_bind(*n);
                    }
                    Value::UInt64(n) => {
                        tuple.push_bind(*n);
                    }
                }
            }
        });
        debug!(sql = %qb.sql(), rows = batch.rows.len(), "mysql insert batch sql");
        let result = qb
            .build()
            .execute(&self.pool)
            .await
            .map_err(RuntimeError::backend)?;

        let rows_written = result.rows_affected();
        debug!(rows_written, "mysql sink batch inserted");
        Ok(WriteReport { rows_written })
    }
}
