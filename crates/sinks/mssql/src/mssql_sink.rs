use std::any::Any;
use std::sync::Arc;

use async_trait::async_trait;
use bb8::Pool;
use bb8_tiberius::ConnectionManager;
use tiberius::{ColumnData, ToSql};
use tracing::info;

use air_elt_commons_mssql::pool;
use air_elt_commons_mssql::types::rowversion::MssqlRowVersionType;
use air_elt_commons_mssql::value_bind::{BoundValue, value_to_column_data};
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{
    Batch, Row, RowOp, Schema, SchemaProvider, SinkCtx, WriteReport, WriteSpec,
};
use air_elt_core::traits::Sink;
use air_elt_core::types::DataType;

use crate::config::model::MssqlSinkConfig;
use crate::sql_statements as sql;

/// Per-write-target context. Built once in `build_context`, reused across
/// all write batches for that target.
pub struct MssqlSinkCtx {
    pub schema: Schema,
    /// Indices into the row's `values` for the columns we insert. The
    /// runner produces a row with as many slots as `WriteSpec::columns`; we
    /// may skip some (ROWVERSION is server-generated and must be excluded).
    insert_value_indices: Vec<usize>,
    /// DataType per insert-column, parallel to `insert_value_indices`.
    insert_column_types: Vec<DataType>,
    /// Pre-built INSERT prefix (`INSERT INTO ... VALUES `).
    insert_sql: String,
    /// Pre-built MERGE prefix (`MERGE ... WITH (HOLDLOCK) AS target USING (VALUES `).
    merge_header: Option<String>,
    /// Pre-built MERGE suffix (`) AS source(...) ON ... WHEN ...;`).
    merge_tail: Option<String>,
    /// Pre-built DELETE prefix (`DELETE FROM ... WHERE k IN (`).
    delete_header: Option<String>,
    /// Indices into `insert_value_indices` (i.e. row.values positions) for
    /// the conflict key columns.
    key_value_indices: Vec<usize>,
    /// DataType per key column.
    key_column_types: Vec<DataType>,
}

impl SinkCtx for MssqlSinkCtx {
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_schema_provider(&self) -> Option<&dyn SchemaProvider> {
        Some(self)
    }
}

impl SchemaProvider for MssqlSinkCtx {
    fn schema(&self) -> &Schema {
        &self.schema
    }
}

pub struct MssqlSink {
    pool: Pool<ConnectionManager>,
}

impl MssqlSink {
    pub async fn connect(config: MssqlSinkConfig) -> RuntimeResult<Self> {
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
        Ok(Self { pool })
    }

    async fn ensure_connection_alive(&self) -> RuntimeResult<()> {
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        conn.simple_query(sql::PING)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }
}

#[async_trait]
impl Sink for MssqlSink {
    async fn validate_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        let probe = sql::probe_insert_where_false(&spec.table, &spec.columns)?;
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        conn.simple_query(&probe)
            .await
            .map_err(RuntimeError::backend)?;
        info!(table = %spec.table, "mssql sink insert access validated");
        Ok(())
    }

    async fn validate_delete_access(&self, spec: &WriteSpec) -> RuntimeResult<()> {
        self.ensure_connection_alive().await?;
        let probe = sql::probe_delete_where_false(&spec.table)?;
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;
        conn.simple_query(&probe)
            .await
            .map_err(RuntimeError::backend)?;
        Ok(())
    }

    async fn describe_schema(&self, table: &str) -> RuntimeResult<Schema> {
        air_elt_commons_mssql::schema::fetch_schema(&self.pool, table).await
    }

    async fn build_context(&self, spec: &WriteSpec) -> RuntimeResult<Arc<dyn SinkCtx>> {
        let schema = self.describe_schema(&spec.table).await?;
        let column_types: Vec<DataType> = spec
            .columns
            .iter()
            .map(|name| {
                schema
                    .find(name)
                    .map(|f| f.data_type.clone())
                    .ok_or_else(|| {
                        RuntimeError::Other(format!(
                            "column {name:?} not in sink schema for {:?}",
                            spec.table
                        ))
                    })
            })
            .collect::<RuntimeResult<Vec<_>>>()?;

        // Strip ROWVERSION columns from the INSERT path. ROWVERSION is
        // server-generated and read-only — any attempt to bind a value
        // for it would error.
        let mut insert_columns: Vec<String> = Vec::with_capacity(spec.columns.len());
        let mut insert_value_indices: Vec<usize> = Vec::with_capacity(spec.columns.len());
        let mut insert_column_types: Vec<DataType> = Vec::with_capacity(spec.columns.len());
        for (i, name) in spec.columns.iter().enumerate() {
            let dt = &column_types[i];
            let is_rowversion =
                matches!(dt, DataType::Custom(ct) if ct.kind() == MssqlRowVersionType::KIND);
            if is_rowversion {
                continue;
            }
            insert_columns.push(name.clone());
            insert_value_indices.push(i);
            insert_column_types.push(dt.clone());
        }

        let insert_sql = sql::insert_prefix(&spec.table, &insert_columns)?;

        let (merge_header, merge_tail, key_value_indices, key_column_types) = match &spec.conflict {
            Some(cfg) => {
                let header = sql::merge_prefix(&spec.table)?;
                let tail = sql::merge_suffix(&insert_columns, &cfg.key, cfg.strategy)?;
                // Key positions inside the *insert_columns* projection.
                let key_positions_in_insert: Vec<usize> = cfg
                    .key
                    .iter()
                    .map(|k| {
                        insert_columns.iter().position(|c| c == k).ok_or_else(|| {
                            RuntimeError::Other(format!(
                                "key column {k:?} not in insert columns (rowversion key?)"
                            ))
                        })
                    })
                    .collect::<RuntimeResult<Vec<_>>>()?;
                // Translate to row.values positions.
                let value_idx: Vec<usize> = key_positions_in_insert
                    .iter()
                    .map(|p| insert_value_indices[*p])
                    .collect();
                let key_types: Vec<DataType> = key_positions_in_insert
                    .iter()
                    .map(|p| insert_column_types[*p].clone())
                    .collect();
                (Some(header), Some(tail), value_idx, key_types)
            }
            None => (None, None, Vec::new(), Vec::new()),
        };

        let delete_header = match &spec.conflict {
            Some(cfg) => Some(sql::delete_prefix(&spec.table, &cfg.key)?),
            None => None,
        };

        Ok(Arc::new(MssqlSinkCtx {
            schema,
            insert_value_indices,
            insert_column_types,
            insert_sql,
            merge_header,
            merge_tail,
            delete_header,
            key_value_indices,
            key_column_types,
        }))
    }

    async fn write_batch(
        &self,
        _spec: &WriteSpec,
        ctx: Arc<dyn SinkCtx>,
        batch: Batch,
        dry_run: bool,
    ) -> RuntimeResult<WriteReport> {
        let my_ctx = ctx.downcast_ref_to::<MssqlSinkCtx>()?;

        let (upserts, deletes): (Vec<_>, Vec<_>) = batch
            .rows
            .iter()
            .partition(|r| matches!(r.op, RowOp::Upsert));

        let mut rows_written: u64 = 0;
        let mut conn = self.pool.get().await.map_err(RuntimeError::backend)?;

        if !upserts.is_empty() {
            let (sql_text, params) = if my_ctx.merge_header.is_some() {
                build_merge(&upserts, my_ctx)?
            } else {
                build_insert(&upserts, my_ctx)?
            };
            let final_sql = if dry_run {
                sql::dry_run_wrap(&sql_text)
            } else {
                sql_text
            };
            let refs: Vec<&dyn ToSql> = params.iter().map(|b| b as &dyn ToSql).collect();
            conn.execute(&final_sql, &refs)
                .await
                .map_err(RuntimeError::backend)?;
            if !dry_run {
                rows_written += upserts.len() as u64;
            }
        }

        if !deletes.is_empty() {
            if my_ctx.delete_header.is_some() {
                let (sql_text, params) = build_delete(&deletes, my_ctx)?;
                let final_sql = if dry_run {
                    sql::dry_run_wrap(&sql_text)
                } else {
                    sql_text
                };
                let refs: Vec<&dyn ToSql> = params.iter().map(|b| b as &dyn ToSql).collect();
                conn.execute(&final_sql, &refs)
                    .await
                    .map_err(RuntimeError::backend)?;
            }
        }

        Ok(WriteReport { rows_written })
    }

    fn cancel_safe(&self) -> bool {
        // tiberius is not cancellation-safe; dropping its futures
        // mid-await can leave the driver in an inconsistent state.
        // The runner spawns + detaches when this returns false.
        false
    }
}

/// Build an `INSERT ... VALUES (@P1, ..), (@PN+1, ..)` statement and the
/// parallel `Vec<BoundValue>` of bound parameters.
fn build_insert(rows: &[&Row], ctx: &MssqlSinkCtx) -> RuntimeResult<(String, Vec<BoundValue>)> {
    let mut sql = ctx.insert_sql.clone();
    let cols_per_row = ctx.insert_value_indices.len();
    let mut params: Vec<BoundValue> = Vec::with_capacity(rows.len() * cols_per_row);

    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for (col_idx, value_idx) in ctx.insert_value_indices.iter().enumerate() {
            if col_idx > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("@P{}", params.len() + 1));
            let cd = bind_cell(&row.values[*value_idx], &ctx.insert_column_types[col_idx])?;
            params.push(BoundValue(cd));
        }
        sql.push(')');
    }
    Ok((sql, params))
}

/// Build a MERGE statement with parameterised VALUES tuples.
fn build_merge(rows: &[&Row], ctx: &MssqlSinkCtx) -> RuntimeResult<(String, Vec<BoundValue>)> {
    let header = ctx
        .merge_header
        .as_ref()
        .ok_or_else(|| RuntimeError::Other("merge header not built".into()))?;
    let tail = ctx
        .merge_tail
        .as_ref()
        .ok_or_else(|| RuntimeError::Other("merge tail not built".into()))?;

    let mut sql = header.clone();
    let cols_per_row = ctx.insert_value_indices.len();
    let mut params: Vec<BoundValue> = Vec::with_capacity(rows.len() * cols_per_row);

    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for (col_idx, value_idx) in ctx.insert_value_indices.iter().enumerate() {
            if col_idx > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&format!("@P{}", params.len() + 1));
            let cd = bind_cell(&row.values[*value_idx], &ctx.insert_column_types[col_idx])?;
            params.push(BoundValue(cd));
        }
        sql.push(')');
    }
    sql.push_str(tail);
    Ok((sql, params))
}

/// Build a DELETE statement with parameterised IN-list values.
fn build_delete(rows: &[&Row], ctx: &MssqlSinkCtx) -> RuntimeResult<(String, Vec<BoundValue>)> {
    let header = ctx
        .delete_header
        .as_ref()
        .ok_or_else(|| RuntimeError::Other("delete header not built".into()))?;

    let single_key = ctx.key_value_indices.len() == 1;
    let mut sql = header.clone();
    let mut params: Vec<BoundValue> = Vec::with_capacity(rows.len() * ctx.key_value_indices.len());

    for (row_idx, row) in rows.iter().enumerate() {
        if row_idx > 0 {
            sql.push_str(", ");
        }
        if single_key {
            sql.push_str(&format!("@P{}", params.len() + 1));
            let value_idx = ctx.key_value_indices[0];
            let cd = bind_cell(&row.values[value_idx], &ctx.key_column_types[0])?;
            params.push(BoundValue(cd));
        } else {
            sql.push('(');
            for (k, &value_idx) in ctx.key_value_indices.iter().enumerate() {
                if k > 0 {
                    sql.push_str(", ");
                }
                sql.push_str(&format!("@P{}", params.len() + 1));
                let cd = bind_cell(&row.values[value_idx], &ctx.key_column_types[k])?;
                params.push(BoundValue(cd));
            }
            sql.push(')');
        }
    }
    sql.push(')');
    Ok((sql, params))
}

fn bind_cell(
    value: &air_elt_core::types::Value,
    dt: &DataType,
) -> RuntimeResult<ColumnData<'static>> {
    value_to_column_data(value, dt)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_commons_mssql::types::rowversion::MssqlRowVersionType;
    use air_elt_core::model::{Field, RowOp, Schema};
    use air_elt_core::types::Value;

    fn ctx_with(columns: Vec<&str>, types: Vec<DataType>, key: Option<Vec<&str>>) -> MssqlSinkCtx {
        let mut all_columns: Vec<String> = Vec::new();
        let mut all_types: Vec<DataType> = Vec::new();
        let mut value_indices: Vec<usize> = Vec::new();
        let fields = columns
            .iter()
            .zip(types.iter())
            .map(|(n, t)| Field {
                name: (*n).to_string(),
                data_type: t.clone(),
                nullable: true,
            })
            .collect();
        let schema = Schema::new(fields);
        for (i, (name, dt)) in columns.iter().zip(types.iter()).enumerate() {
            let is_rowversion =
                matches!(dt, DataType::Custom(ct) if ct.kind() == MssqlRowVersionType::KIND);
            if !is_rowversion {
                all_columns.push((*name).to_string());
                all_types.push(dt.clone());
                value_indices.push(i);
            }
        }
        let insert_sql = sql::insert_prefix("dbo.t", &all_columns).unwrap();
        let (header, tail, key_idx, key_types) = match key {
            Some(k) => {
                let key_names: Vec<String> = k.iter().map(|s| (*s).to_string()).collect();
                let header = sql::merge_prefix("dbo.t").unwrap();
                let tail = sql::merge_suffix(&all_columns, &key_names, ConflictStrategy::Overwrite)
                    .unwrap();
                let positions: Vec<usize> = key_names
                    .iter()
                    .map(|kn| all_columns.iter().position(|c| c == kn).unwrap())
                    .collect();
                let value_idx: Vec<usize> = positions.iter().map(|p| value_indices[*p]).collect();
                let key_types: Vec<DataType> =
                    positions.iter().map(|p| all_types[*p].clone()).collect();
                (Some(header), Some(tail), value_idx, key_types)
            }
            None => (None, None, vec![], vec![]),
        };
        MssqlSinkCtx {
            schema,
            insert_value_indices: value_indices,
            insert_column_types: all_types,
            insert_sql,
            merge_header: header,
            merge_tail: tail,
            delete_header: None,
            key_value_indices: key_idx,
            key_column_types: key_types,
        }
    }

    use air_elt_core::config::conflict::ConflictStrategy;

    #[test]
    fn insert_sql_uses_at_p_placeholders() {
        let ctx = ctx_with(
            vec!["id", "name"],
            vec![DataType::Int32, DataType::Text { size: None }],
            None,
        );
        let rows = [
            Row {
                values: vec![Value::Int32(1), Value::Text("a".into())],
                op: RowOp::Upsert,
            },
            Row {
                values: vec![Value::Int32(2), Value::Text("b".into())],
                op: RowOp::Upsert,
            },
        ];
        let refs: Vec<&Row> = rows.iter().collect();
        let (sql, params) = build_insert(&refs, &ctx).unwrap();
        assert!(sql.contains("@P1"));
        assert!(sql.contains("@P4"));
        assert_eq!(params.len(), 4);
    }

    #[test]
    fn rowversion_is_filtered_in_insert() {
        let types = vec![
            DataType::Int32,
            DataType::Text { size: None },
            DataType::Custom(Box::new(MssqlRowVersionType)),
        ];
        let ctx = ctx_with(vec!["id", "name", "rv"], types, None);
        let row = Row {
            values: vec![Value::Int32(1), Value::Text("x".into()), Value::Null],
            op: RowOp::Upsert,
        };
        let refs = vec![&row];
        let (sql, params) = build_insert(&refs, &ctx).unwrap();
        // Should contain id and name but not rv.
        assert!(sql.contains("\"id\""));
        assert!(sql.contains("\"name\""));
        assert!(!sql.contains("\"rv\""));
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn merge_sql_uses_holdlock_and_at_p() {
        let ctx = ctx_with(
            vec!["id", "name"],
            vec![DataType::Int32, DataType::Text { size: None }],
            Some(vec!["id"]),
        );
        let row = Row {
            values: vec![Value::Int32(7), Value::Text("z".into())],
            op: RowOp::Upsert,
        };
        let refs = vec![&row];
        let (sql, _params) = build_merge(&refs, &ctx).unwrap();
        assert!(sql.contains("WITH (HOLDLOCK)"));
        assert!(sql.contains("@P1"));
        assert!(sql.contains("WHEN MATCHED"));
    }
}
