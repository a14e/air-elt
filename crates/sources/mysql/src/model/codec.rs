use std::str::FromStr;

use bigdecimal::BigDecimal;
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
        DataType::Int8 => {
            nullable::<i8>(row, index).map(|o| o.map(Value::Int8).unwrap_or(Value::Null))
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
        DataType::UInt8 => {
            nullable::<u8>(row, index).map(|o| o.map(Value::UInt8).unwrap_or(Value::Null))
        }
        DataType::UInt16 => {
            nullable::<u16>(row, index).map(|o| o.map(Value::UInt16).unwrap_or(Value::Null))
        }
        DataType::UInt32 => {
            nullable::<u32>(row, index).map(|o| o.map(Value::UInt32).unwrap_or(Value::Null))
        }
        DataType::UInt64 => {
            nullable::<u64>(row, index).map(|o| o.map(Value::UInt64).unwrap_or(Value::Null))
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
        // MySQL has no native IP type — these arms exist only for
        // pipelines that explicitly declare `DataType::Ipv4`/`Ipv6`
        // against a VARCHAR/TEXT column. We decode the cell as text
        // and parse via std::net::Ipv*Addr.
        DataType::Ipv4 => match nullable::<String>(row, index)? {
            None => Ok(Value::Null),
            Some(s) => std::net::Ipv4Addr::from_str(s.trim())
                .map(Value::Ipv4)
                .map_err(|e| {
                    RuntimeError::Other(format!("invalid IPv4 text in mysql column: {e}"))
                }),
        },
        DataType::Ipv6 => match nullable::<String>(row, index)? {
            None => Ok(Value::Null),
            Some(s) => std::net::Ipv6Addr::from_str(s.trim())
                .map(Value::Ipv6)
                .map_err(|e| {
                    RuntimeError::Other(format!("invalid IPv6 text in mysql column: {e}"))
                }),
        },
        // MySQL/MariaDB `decimal(p, 0)` arrives as `BigDecimal`. Force-
        // rescale to 0 before extracting so a future sqlx normalisation of
        // values like `1000` (potentially surfacing as `1e3`) doesn't trip
        // a strict exponent check. See pg counterpart for context.
        DataType::BigInt { .. } => match nullable::<BigDecimal>(row, index)? {
            None => Ok(Value::Null),
            Some(d) => {
                let (mantissa, _) = d.with_scale(0).into_bigint_and_exponent();
                Ok(Value::BigInt(mantissa))
            }
        },
        DataType::Decimal { .. } => match nullable::<BigDecimal>(row, index)? {
            None => Ok(Value::Null),
            Some(d) => Ok(Value::Decimal(d)),
        },
        // MySQL has no native xml type; pipelines reach this arm only when
        // the operator declares a text column as `DataType::Xml` upstream
        // (or a future MySQL version surfaces an xml-like type). Decode as
        // text — the canonical XML payload lives in `Value::Text`.
        DataType::Xml => {
            nullable::<String>(row, index).map(|o| o.map(Value::Text).unwrap_or(Value::Null))
        }
        // SQL sources never produce Union — only Mongo's sample-based
        // inference does. Mirrors the mysql sink's `unreachable!` for
        // the same structural invariant.
        DataType::Object => unreachable!("mysql sources never produce Object types"),
        DataType::Union(_) => unreachable!("mysql sources never produce Union types"),
        DataType::Custom(_) => unreachable!(
            "DataType::Custom must be handled by the connector before reaching decode_column"
        ),
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
        Value::Int8(n) => query.bind(*n),
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
        Value::Ipv4(a) => query.bind(a.to_string()),
        Value::Ipv6(a) => query.bind(a.to_string()),
        Value::Json(j) => query.bind(j),
        Value::BigInt(b) => query.bind(BigDecimal::new(b.clone(), 0)),
        Value::Decimal(d) => query.bind(d.clone()),
        Value::UInt8(n) => query.bind(*n),
        Value::UInt16(n) => query.bind(*n),
        Value::UInt32(n) => query.bind(*n),
        Value::UInt64(n) => query.bind(*n),
        Value::Object(_) => {
            unreachable!(
                "Value::Object cannot appear in cursor values — it is not cursor-compatible"
            )
        }
        Value::Custom(_) => unreachable!(
            "Value::Custom must be handled by the connector before reaching bind_cursor_value"
        ),
    }
}
