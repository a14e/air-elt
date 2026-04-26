use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;
use sqlx::mysql::MySqlRow;
use uuid::Uuid;

use air_elt_commons_mysql::null_bind;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::types::{DataType, Value};

/// Decode a single sqlx column into the canonical `Value`.
pub fn decode_column(row: &MySqlRow, index: usize, data_type: DataType) -> RuntimeResult<Value> {
    match data_type {
        DataType::Bool => {
            // MySQL `tinyint(1)` decodes as bool via sqlx.
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
        // MariaDB 10.7+ has a native UUID column type; stock MySQL does not.
        // sqlx-mysql exposes the column metadata as `BINARY` but MariaDB
        // sends the value as the 36-char canonical text form on the wire,
        // not as 16 raw bytes. Decode as `Vec<u8>` and branch on length:
        // 16 → raw uuid bytes, 36 → ascii canonical form.
        DataType::Uuid => match nullable::<Vec<u8>>(row, index)? {
            None => Ok(Value::Null),
            Some(bytes) => parse_uuid_bytes(&bytes).map(Value::Uuid),
        },
        DataType::Json => nullable::<serde_json::Value>(row, index)
            .map(|o| o.map(Value::Json).unwrap_or(Value::Null)),
    }
}

fn parse_uuid_bytes(bytes: &[u8]) -> RuntimeResult<Uuid> {
    match bytes.len() {
        16 => {
            let arr: [u8; 16] = bytes.try_into().expect("len checked");
            Ok(Uuid::from_bytes(arr))
        }
        36 => {
            let s = std::str::from_utf8(bytes).map_err(RuntimeError::backend)?;
            Uuid::parse_str(s).map_err(RuntimeError::backend)
        }
        other => Err(RuntimeError::Other(format!(
            "uuid column returned {other} bytes; expected 16 (binary) or 36 (canonical text)"
        ))),
    }
}

fn nullable<'r, T>(row: &'r MySqlRow, index: usize) -> RuntimeResult<Option<T>>
where
    T: sqlx::Decode<'r, sqlx::MySql> + sqlx::Type<sqlx::MySql>,
{
    row.try_get::<Option<T>, _>(index)
        .map_err(RuntimeError::backend)
}

/// Bind a `Value` into a sqlx query for cursor comparisons.
pub fn bind_cursor_value<'q>(
    query: sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments>,
    value: &'q Value,
    dt: DataType,
) -> sqlx::query::Query<'q, sqlx::MySql, sqlx::mysql::MySqlArguments> {
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
        Value::Uuid(u) => query.bind(u.to_string()),
        Value::Json(j) => query.bind(j),
    }
}
