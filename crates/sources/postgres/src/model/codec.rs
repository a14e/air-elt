use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use air_elt_commons_pg::null_bind;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::types::{DataType, Value};

/// Decode a single sqlx column into the canonical `Value`.
///
/// The `data_type` argument comes from the pre-computed `Schema` — we don't
/// inspect the row type at runtime because types are known ahead of time and
/// decoding is hot-path.
pub fn decode_column(row: &PgRow, index: usize, data_type: DataType) -> RuntimeResult<Value> {
    match data_type {
        DataType::Bool => {
            nullable::<bool>(row, index).map(|o| o.map(Value::Bool).unwrap_or(Value::Null))
        }
        DataType::Int16 => {
            nullable::<i16>(row, index).map(|o| o.map(Value::Int16).unwrap_or(Value::Null))
        }
        DataType::Int32 => {
            nullable::<i32>(row, index).map(|o| o.map(Value::Int32).unwrap_or(Value::Null))
        }
        DataType::Int64 => {
            nullable::<i64>(row, index).map(|o| o.map(Value::Int64).unwrap_or(Value::Null))
        }
        DataType::Float32 => {
            nullable::<f32>(row, index).map(|o| o.map(Value::Float32).unwrap_or(Value::Null))
        }
        DataType::Float64 => {
            nullable::<f64>(row, index).map(|o| o.map(Value::Float64).unwrap_or(Value::Null))
        }
        DataType::Text { .. } => {
            nullable::<String>(row, index).map(|o| o.map(Value::Text).unwrap_or(Value::Null))
        }
        DataType::Bytes { .. } => {
            nullable::<Vec<u8>>(row, index).map(|o| o.map(Value::Bytes).unwrap_or(Value::Null))
        }
        DataType::Date => {
            nullable::<NaiveDate>(row, index).map(|o| o.map(Value::Date).unwrap_or(Value::Null))
        }
        DataType::Timestamp => nullable::<DateTime<Utc>>(row, index)
            .map(|o| o.map(Value::Timestamp).unwrap_or(Value::Null)),
        DataType::Uuid => {
            nullable::<Uuid>(row, index).map(|o| o.map(Value::Uuid).unwrap_or(Value::Null))
        }
        DataType::Json => nullable::<serde_json::Value>(row, index)
            .map(|o| o.map(Value::Json).unwrap_or(Value::Null)),
    }
}

fn nullable<'r, T>(row: &'r PgRow, index: usize) -> RuntimeResult<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get::<Option<T>, _>(index)
        .map_err(RuntimeError::backend)
}

/// Bind a `Value` into a sqlx query for cursor comparisons.
///
/// NULL values are bound as typed NULLs (correct wire OID) via the shared
/// `null_bind` helper. Non-null values bind as the canonical type — sqlx's
/// Postgres layer handles any safe widening.
pub fn bind_cursor_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
    value: &'q Value,
    dt: DataType,
) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
    match value {
        Value::Null => null_bind::bind_typed_null(query, dt),
        Value::Bool(b) => query.bind(*b),
        Value::Int16(n) => query.bind(*n),
        Value::Int32(n) => query.bind(*n),
        Value::Int64(n) => query.bind(*n),
        Value::Float32(n) => query.bind(*n),
        Value::Float64(n) => query.bind(*n),
        Value::Text(s) => query.bind(s.as_str()),
        Value::Bytes(b) => query.bind(b.as_slice()),
        Value::Date(d) => query.bind(*d),
        Value::Timestamp(ts) => query.bind(*ts),
        Value::Uuid(u) => query.bind(*u),
        Value::Json(j) => query.bind(j),
    }
}
