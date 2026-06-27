use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;
use uuid::Uuid;

use air_elt_commons_pg::null_bind;
use air_elt_commons_pg::types::{PgHllType, PgHllValue, PgInetType, PgInetValue};
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
        // Canonical IPv4/IPv6 columns are unusual on the PG side
        // (the canonical pivot for `inet` is `PgInetType` — see
        // below). These arms still need to exist because a user can
        // declare an `inet` column in the source schema and map it
        // through a `convert` to `Ipv4`/`Ipv6` before the data
        // reaches the sink — but for plain reads the schema
        // discriminator routes to the Custom(PgInetType) arm. We
        // decode via IpAddr and emit the matching variant.
        DataType::Ipv4 | DataType::Ipv6 => {
            match nullable::<sqlx::types::ipnetwork::IpNetwork>(row, index)? {
                None => Ok(Value::Null),
                Some(sqlx::types::ipnetwork::IpNetwork::V4(n)) => Ok(Value::Ipv4(n.ip())),
                Some(sqlx::types::ipnetwork::IpNetwork::V6(n)) => Ok(Value::Ipv6(n.ip())),
            }
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
        DataType::Object => unreachable!("postgres sources never produce Object types"),
        DataType::Union(_) => unreachable!("postgres sources never produce Union types"),
        DataType::Interval => {
            unreachable!("postgres sources never produce Interval (redis-only type)")
        }
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
        // PG `inet`: decode as IpNetwork (preserves the netmask
        // losslessly) and wrap in PgInetValue. Downstream conversions
        // to canonical Ipv4/Ipv6 are mask-dropping and gated on
        // `truncate=true` in the convert dispatcher.
        DataType::Custom(t) if t.kind() == PgInetType::KIND => {
            match nullable::<sqlx::types::ipnetwork::IpNetwork>(row, index)? {
                None => Ok(Value::Null),
                Some(net) => Ok(Value::Custom(Box::new(PgInetValue(net)))),
            }
        }
        // Native array column: decode `Vec<Option<NativeT>>` for the
        // element type and rebuild a `Value::Array` of canonical elements.
        // A NULL array column yields `Value::Null`; a NULL element yields
        // `Value::Null` inside the array (PG arrays permit NULL elements).
        DataType::Array { element, .. } => decode_array(row, index, element.as_deref()),
        DataType::Custom(t) => Err(RuntimeError::Other(format!(
            "postgres source has no decoder for custom type {kind:?}",
            kind = t.kind()
        ))),
    }
}

/// Decode a native PG array column into `Value::Array`. Dispatches on the
/// declared element type (from the pre-computed schema) and reads
/// `Vec<Option<NativeT>>`, mapping each cell to a canonical `Value`
/// (`None` → `Value::Null`).
fn decode_array(row: &PgRow, index: usize, element: Option<&DataType>) -> RuntimeResult<Value> {
    match element {
        Some(DataType::Bool) => decode_array_with(row, index, |b: bool| Value::Bool(b)),
        Some(DataType::Int16) => decode_array_with(row, index, |n: i16| Value::Int16(n)),
        Some(DataType::Int32) => decode_array_with(row, index, |n: i32| Value::Int32(n)),
        Some(DataType::Int64) => decode_array_with(row, index, |n: i64| Value::Int64(n)),
        Some(DataType::Float32) => decode_array_with(row, index, |n: f32| Value::Float32(n)),
        Some(DataType::Float64) => decode_array_with(row, index, |n: f64| Value::Float64(n)),
        Some(DataType::Text { .. }) => decode_array_with(row, index, Value::Text),
        Some(DataType::Date) => decode_array_with(row, index, |d: NaiveDate| Value::Date(d)),
        Some(DataType::Timestamp) => {
            decode_array_with(row, index, |ts: DateTime<Utc>| Value::Timestamp(ts))
        }
        Some(DataType::Uuid) => decode_array_with(row, index, |u: Uuid| Value::Uuid(u)),
        // numeric[] surfaces as `BigDecimal`. A `numeric(p, 0)` array maps
        // canonically to BigInt; everything else stays Decimal. The schema
        // already resolved the element variant, so honour it here.
        Some(DataType::BigInt { .. }) => decode_array_with(row, index, |d: BigDecimal| {
            let (mantissa, _) = d.with_scale(0).into_bigint_and_exponent();
            Value::BigInt(mantissa)
        }),
        Some(DataType::Decimal { .. }) => {
            decode_array_with(row, index, |d: BigDecimal| Value::Decimal(d))
        }
        // The pg schema introspector only ever produces array columns with
        // the element types above (`PgType::is_array_element`). Any other
        // element — or an unknown element on a real column — is a structural
        // bug in introspection, not a runtime data shape.
        other => Err(RuntimeError::Other(format!(
            "postgres source has no array decoder for element type {other:?}"
        ))),
    }
}

/// Read a `Vec<Option<NativeT>>` array column and map each element to a
/// canonical `Value` via `to_value` (`None` → `Value::Null`). A NULL array
/// column itself yields `Value::Null`.
fn decode_array_with<'r, NativeT>(
    row: &'r PgRow,
    index: usize,
    to_value: impl Fn(NativeT) -> Value,
) -> RuntimeResult<Value>
where
    NativeT: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    Vec<Option<NativeT>>: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    match nullable::<Vec<Option<NativeT>>>(row, index)? {
        None => Ok(Value::Null),
        Some(items) => {
            let values = items
                .into_iter()
                .map(|item| item.map(&to_value).unwrap_or(Value::Null))
                .collect();
            Ok(Value::Array(values))
        }
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
        Value::Ipv4(a) => {
            let net = sqlx::types::ipnetwork::IpNetwork::new(std::net::IpAddr::V4(*a), 32)
                .expect("/32 prefix always valid");
            query.bind(net)
        }
        Value::Ipv6(a) => {
            let net = sqlx::types::ipnetwork::IpNetwork::new(std::net::IpAddr::V6(*a), 128)
                .expect("/128 prefix always valid");
            query.bind(net)
        }
        Value::Json(j) => query.bind(j),
        Value::BigInt(b) => query.bind(BigDecimal::new(b.clone(), 0)),
        Value::Decimal(d) => query.bind(d.clone()),
        Value::Object(_) => {
            unreachable!(
                "Value::Object cannot appear in cursor values — it is not cursor-compatible"
            )
        }
        Value::UInt8(_) | Value::UInt16(_) | Value::UInt32(_) | Value::UInt64(_) => {
            unreachable!("postgres has no unsigned int columns; cursor cannot carry unsigned")
        }
        Value::Interval(_) => {
            unreachable!("Value::Interval (redis-only type) is not cursor-compatible")
        }
        Value::Array(_) => {
            unreachable!("Value::Array is not cursor-compatible (rejected by Key)")
        }
        Value::Custom(c) => {
            // PG `inet` is cursor-compatible — wrap the IpNetwork
            // directly. Other custom types (HLL) are not cursor-
            // compatible and validation rejects them upstream.
            if let Some(inet) = c.as_any().downcast_ref::<PgInetValue>() {
                query.bind(inet.0)
            } else {
                unreachable!(
                    "Value::Custom must be handled by the connector before reaching bind_cursor_value"
                )
            }
        }
    }
}
