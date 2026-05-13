//! Row decode for the MS SQL source connector via tiberius `Row`.

use bigdecimal::{BigDecimal, FromPrimitive};
use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use num_bigint::BigInt;
use tiberius::Row;
use tiberius::numeric::Numeric;
use uuid::Uuid;

use air_elt_commons_mssql::types::image::{MssqlImageType, MssqlImageValue};
use air_elt_commons_mssql::types::rowversion::{MssqlRowVersionType, MssqlRowVersionValue};
use air_elt_commons_mssql::types::time::{MssqlTimeType, MssqlTimeValue};
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::types::{DataType, Value};

/// Decode a tiberius `Row` column at position `index` into a `Value`.
pub fn decode_column(row: &Row, index: usize, data_type: &DataType) -> RuntimeResult<Value> {
    match data_type {
        DataType::Bool => {
            let v: Option<bool> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Bool).unwrap_or(Value::Null))
        }
        DataType::Int16 => {
            let v: Option<i16> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Int16).unwrap_or(Value::Null))
        }
        DataType::Int32 => {
            let v: Option<i32> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Int32).unwrap_or(Value::Null))
        }
        DataType::Int64 => {
            let v: Option<i64> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Int64).unwrap_or(Value::Null))
        }
        DataType::UInt8 => {
            let v: Option<u8> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::UInt8).unwrap_or(Value::Null))
        }
        DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            // MS SQL has no unsigned types beyond TINYINT. These are
            // widened by the matrix from MySQL sources.
            let v: Option<i64> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Int64).unwrap_or(Value::Null))
        }
        DataType::Int8 => {
            // mssql_type never emits Int8 (MSSQL TINYINT is unsigned 0..=255,
            // mapped to UInt8). This arm is unreachable from schema-driven
            // decode; if Transform ever requests narrowing it should compile
            // a `Convert` op rather than ask the codec to do it.
            Err(RuntimeError::Other(
                "mssql source never emits Int8; transform layer owns narrowing".into(),
            ))
        }
        DataType::Float32 => {
            let v: Option<f32> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Float32).unwrap_or(Value::Null))
        }
        DataType::Float64 => {
            let v: Option<f64> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Float64).unwrap_or(Value::Null))
        }
        DataType::Text { .. } | DataType::Xml => {
            let v: Option<&str> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(|s| Value::Text(s.to_string())).unwrap_or(Value::Null))
        }
        DataType::Bytes { .. } => {
            let v: Option<&[u8]> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(|b| Value::Bytes(b.to_vec())).unwrap_or(Value::Null))
        }
        DataType::Date => {
            let v: Option<NaiveDate> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Date).unwrap_or(Value::Null))
        }
        DataType::Timestamp => {
            let v: Option<NaiveDateTime> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(|nd| Value::Timestamp(nd.and_utc()))
                .unwrap_or(Value::Null))
        }
        DataType::Uuid => {
            let v: Option<Uuid> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(Value::Uuid).unwrap_or(Value::Null))
        }
        DataType::Json => {
            // No native JSON type in mssql; if a flow ever maps through Json
            // we read it as text and parse it.
            let v: Option<&str> = row.try_get(index).map_err(RuntimeError::backend)?;
            match v {
                Some(s) => {
                    let j: serde_json::Value = serde_json::from_str(s)
                        .map_err(|e| RuntimeError::Other(format!("json decode: {e}")))?;
                    Ok(Value::Json(j))
                }
                None => Ok(Value::Null),
            }
        }
        DataType::BigInt { .. } => decode_bigint(row, index),
        DataType::Decimal { scale, .. } => decode_decimal(row, index, *scale),
        DataType::Custom(ct) if ct.kind() == MssqlImageType::KIND => {
            let v: Option<&[u8]> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(
                v.map(|b| Value::Custom(Box::new(MssqlImageValue(b.to_vec()))))
                    .unwrap_or(Value::Null),
            )
        }
        DataType::Custom(ct) if ct.kind() == MssqlRowVersionType::KIND => {
            let v: Option<&[u8]> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(
                v.map(|b| Value::Custom(Box::new(MssqlRowVersionValue(b.to_vec()))))
                    .unwrap_or(Value::Null),
            )
        }
        DataType::Custom(ct) if ct.kind() == MssqlTimeType::KIND => {
            let v: Option<NaiveTime> = row.try_get(index).map_err(RuntimeError::backend)?;
            Ok(v.map(|t| Value::Custom(Box::new(MssqlTimeValue(t))))
                .unwrap_or(Value::Null))
        }
        DataType::Custom(_) => Err(RuntimeError::Other(format!(
            "mssql source cannot decode Custom type: {data_type:?}"
        ))),
        DataType::Union(_) => Err(RuntimeError::Other(
            "mssql source cannot decode Union type".into(),
        )),
    }
}

/// Decode `BIGINT` columns and (rarely) wider `numeric(p, 0)`.
fn decode_bigint(row: &Row, index: usize) -> RuntimeResult<Value> {
    // Try `Numeric` (server-reported numeric/decimal) first.
    let as_numeric: Result<Option<Numeric>, _> = row.try_get(index);
    if let Ok(opt) = as_numeric {
        return Ok(opt
            .map(|n| Value::BigInt(BigInt::from(n.value())))
            .unwrap_or(Value::Null));
    }
    // Fall back to i64 (native BIGINT).
    let v: Option<i64> = row.try_get(index).map_err(RuntimeError::backend)?;
    Ok(v.map(|n| Value::BigInt(BigInt::from(n)))
        .unwrap_or(Value::Null))
}

/// Decode `DECIMAL/NUMERIC` and money types. Tiberius returns
/// `tiberius::numeric::Numeric` (i128 coefficient + u8 scale). Money types
/// are returned as either `Numeric` or `f64` depending on protocol — we try
/// `Numeric` first and fall back to `f64`, snapping the f64 to the column's
/// declared scale (from `INFORMATION_SCHEMA`).
fn decode_decimal(row: &Row, index: usize, declared_scale: Option<u32>) -> RuntimeResult<Value> {
    let as_numeric: Result<Option<Numeric>, _> = row.try_get(index);
    if let Ok(opt) = as_numeric {
        return Ok(opt
            .map(|n| {
                let scale = i64::from(n.scale());
                let coef = BigInt::from(n.value());
                Value::Decimal(BigDecimal::new(coef, scale))
            })
            .unwrap_or(Value::Null));
    }
    // Money / SmallMoney sometimes surface as f64 on older protocols. Use the
    // declared scale (MONEY=4, SMALLMONEY=4, DECIMAL(p,s)=s) so f64-fallback
    // doesn't silently truncate non-money decimals.
    let target_scale = declared_scale.map(i64::from).unwrap_or(4);
    let v: Option<f64> = row.try_get(index).map_err(RuntimeError::backend)?;
    match v {
        Some(f) => match BigDecimal::from_f64(f) {
            Some(d) => Ok(Value::Decimal(d.with_scale(target_scale))),
            None => Err(RuntimeError::Other(format!(
                "mssql decimal: f64 value {f} cannot be represented as BigDecimal \
                 (NaN/Infinity?) — refusing to silently emit NULL"
            ))),
        },
        None => Ok(Value::Null),
    }
}
