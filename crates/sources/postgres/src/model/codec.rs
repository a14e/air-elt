use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use air_elt_commons_pg::null_bind;
use air_elt_commons_pg::types::{PgHllType, PgHllValue};
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
        // PG `xml` columns stream as text on the wire — sqlx has no native
        // XmlValue type. The XML payload lives in `Value::Text`; the
        // `DataType::Xml` tag carries the schema-level distinction.
        DataType::Xml => {
            nullable::<String>(row, index).map(|o| o.map(Value::Text).unwrap_or(Value::Null))
        }
        // BigInt and Decimal are both stored as `numeric` in pg, so sqlx
        // surfaces them through `BigDecimal`. The column's declared scale
        // is 0 (schema invariant) but the wire-level `dscale` may not
        // exactly match for values like `1000` if a future sqlx normalises
        // them. Force-rescale to 0 first so the integer mantissa we extract
        // is the canonical column value regardless of normalisation.
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
        // Postgres has no unsigned int columns and no int1 — these `DataType`
        // variants exist only on the MySQL/MariaDB side. The pg source schema
        // introspector never emits them, so these arms are structurally
        // unreachable.
        DataType::Int8 => unreachable!("postgres has no int1 (signed 8-bit) type"),
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            unreachable!("postgres has no unsigned integer types")
        }
        // SQL sources never produce Union — only Mongo's sample-based
        // inference does. The validation matrix already rejects Union
        // sinks/sources mismatches pre-runtime, so reaching this arm
        // means the schema introspector itself emitted Union (impossible
        // for pg `information_schema`). Mirrors the pg sink's
        // `unreachable!` for the same invariant.
        DataType::Union(_) => unreachable!("postgres sources never produce Union types"),
        // HLL: the wire shape is a binary blob — sqlx decodes it as
        // `Vec<u8>` (sqlx has no native registration for the HLL type
        // OID, but `bytea`-shaped types decode fine through the binary
        // codec). We wrap the bytes in `PgHllValue` so the sink path
        // can re-emit them under an `::hll` cast.
        DataType::Custom(t) if t.kind() == PgHllType::KIND => {
            match nullable::<Vec<u8>>(row, index)? {
                None => Ok(Value::Null),
                Some(bytes) => Ok(Value::Custom(Box::new(PgHllValue(bytes)))),
            }
        }
        DataType::Custom(t) => Err(RuntimeError::Other(format!(
            "postgres source has no decoder for custom type {kind:?}",
            kind = t.kind()
        ))),
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
        // Postgres has no int1; widen to i16.
        Value::Int8(n) => query.bind(*n as i16),
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
        Value::BigInt(b) => query.bind(BigDecimal::new(b.clone(), 0)),
        Value::Decimal(d) => query.bind(d.clone()),
        Value::UInt8(_) | Value::UInt16(_) | Value::UInt32(_) | Value::UInt64(_) => {
            unreachable!("postgres has no unsigned int columns; cursor cannot carry unsigned")
        }
        Value::Custom(_) => unreachable!(
            "Value::Custom must be handled by the connector before reaching bind_cursor_value"
        ),
    }
}
