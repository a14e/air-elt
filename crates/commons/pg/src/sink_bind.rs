//! Postgres sink-side `Separated` binding.
//!
//! Used by:
//! * the insert path inside `push_values` lambda
//! * the DELETE path inside `push_tuples` lambda
//! * the single-key DELETE path that builds a flat `IN (...)` list via
//!   `QueryBuilder::separated`
//!
//! All three call into a `Separated`-shaped binder. The arms are the
//! same as `null_bind::bind_typed_null` (which targets a `Query`); the
//! lifetimes diverge enough that the two cannot be merged generically
//! without either pulling in HKT or boxing the binder.
//!
//! Keep the two arm sets in lockstep — adding a `DataType` variant
//! must update both files.

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Postgres;
use sqlx::query_builder::Separated;
use uuid::Uuid;

use air_elt_core::types::{DataType, Value};

use crate::types::{PgHllType, PgHllValue, PgInetType, PgInetValue};

pub fn bind_value_separated(sep: &mut Separated<'_, '_, Postgres, &str>, v: &Value, dt: &DataType) {
    // Fraud-detector (anti-shortcut #6): SQL sinks can only consume
    // canonical `Value::*` variants (incl. `Value::Json`). Custom row
    // payloads (e.g. `mongodb.bson_object`) must be matrix-converted to
    // `Value::Json` BEFORE reaching bind. Postgres has one legitimate
    // exception — `postgresql.hll`, which is bound natively below. Any
    // other Custom value reaching this function is a missing matrix
    // conversion and would otherwise fall through to `unreachable!()`
    // at runtime. Trip loudly in debug builds; release behaviour is
    // unchanged (the existing `unreachable!()` arm still fires).
    #[cfg(debug_assertions)]
    {
        if let Value::Custom(c) = v {
            let dt = c.dyn_type();
            let kind = dt.kind();
            if kind != PgHllType::KIND && kind != PgInetType::KIND {
                panic!(
                    "SQL sink received unexpected Value::Custom(kind={kind}); matrix conversion to Json missing"
                );
            }
        }
    }
    match v {
        Value::Null => match dt {
            DataType::Int64 => {
                sep.push_bind::<Option<i64>>(None);
            }
            DataType::Bool => {
                sep.push_bind::<Option<bool>>(None);
            }
            DataType::Int8 => {
                // Postgres has no int1 type; bind as int2 (i16).
                sep.push_bind::<Option<i16>>(None);
            }
            DataType::Int16 => {
                sep.push_bind::<Option<i16>>(None);
            }
            DataType::Int32 => {
                sep.push_bind::<Option<i32>>(None);
            }
            DataType::Float32 => {
                sep.push_bind::<Option<f32>>(None);
            }
            DataType::Float64 => {
                sep.push_bind::<Option<f64>>(None);
            }
            DataType::Text { .. } => {
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Bytes { .. } => {
                sep.push_bind::<Option<Vec<u8>>>(None);
            }
            DataType::Date => {
                sep.push_bind::<Option<NaiveDate>>(None);
            }
            DataType::Timestamp => {
                sep.push_bind::<Option<DateTime<Utc>>>(None);
            }
            DataType::Uuid => {
                sep.push_bind::<Option<Uuid>>(None);
            }
            DataType::Ipv4 | DataType::Ipv6 => {
                // sqlx-postgres binds canonical IPv4/IPv6 against the
                // `inet` OID by routing through `IpNetwork` host bits.
                sep.push_bind::<Option<sqlx::types::ipnetwork::IpNetwork>>(None);
            }
            DataType::Json | DataType::Object => {
                sep.push_bind::<Option<serde_json::Value>>(None);
            }
            DataType::BigInt { .. } | DataType::Decimal { .. } => {
                sep.push_bind::<Option<BigDecimal>>(None);
            }
            DataType::Xml => {
                // sqlx cannot infer the `xml` type from a NULL bind; pair the
                // typed-NULL string with an explicit `::xml` cast so the
                // placeholder lands as `$N::xml` and PG accepts it for an
                // `xml` column.
                sep.push_bind::<Option<String>>(None);
                sep.push_unseparated("::xml");
            }
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                unreachable!("postgres has no unsigned integer column types")
            }
            DataType::Union(_) => unreachable!("postgres sinks never carry Union types"),
            DataType::Interval => {
                unreachable!("postgres sinks never carry Interval (redis-only type)")
            }
            // HLL NULL: bind a typed NULL Vec<u8> and tack on `::hll` so
            // the placeholder lands as `$N::hll`. sqlx cannot infer a
            // type for NULL, and PG itself will not accept an untyped
            // NULL into an `hll` column even when the column is
            // nullable. Same shape as the non-null arm below — the only
            // difference is the bound payload.
            DataType::Custom(t) if t.kind() == PgHllType::KIND => {
                sep.push_bind::<Option<Vec<u8>>>(None);
                sep.push_unseparated("::hll");
            }
            DataType::Custom(t) if t.kind() == PgInetType::KIND => {
                sep.push_bind::<Option<sqlx::types::ipnetwork::IpNetwork>>(None);
            }
            DataType::Custom(_) => unreachable!(
                "DataType::Custom must be handled by the connector before reaching sink_bind"
            ),
        },
        Value::Bool(b) => {
            sep.push_bind(*b);
        }
        Value::Int8(n) => {
            // Postgres has no int1 type; widen to int2 (i16) automatically.
            sep.push_bind(*n as i16);
        }
        Value::Int16(n) => {
            sep.push_bind(*n);
        }
        Value::Int32(n) => {
            sep.push_bind(*n);
        }
        Value::Int64(n) => {
            sep.push_bind(*n);
        }
        Value::Float32(n) => {
            sep.push_bind(*n);
        }
        Value::Float64(n) => {
            sep.push_bind(*n);
        }
        Value::Text(s) => {
            sep.push_bind(s.clone());
            // The canonical `Value::Xml` slot is folded into `Value::Text`;
            // the column-level distinction lives only on `dt`. When the
            // sink column is `xml`, pair the text bind with an explicit
            // `::xml` cast so PG accepts the literal.
            if matches!(dt, DataType::Xml) {
                sep.push_unseparated("::xml");
            }
        }
        Value::Bytes(b) => {
            sep.push_bind(b.clone());
        }
        Value::Date(d) => {
            sep.push_bind(*d);
        }
        Value::Timestamp(ts) => {
            sep.push_bind(*ts);
        }
        Value::Uuid(u) => {
            sep.push_bind(*u);
        }
        Value::Ipv4(a) => {
            // Bind through IpNetwork host (/32) — sqlx-postgres
            // recognises this as the `inet` OID payload.
            let net = sqlx::types::ipnetwork::IpNetwork::new(std::net::IpAddr::V4(*a), 32)
                .expect("/32 prefix always valid for any IPv4 address");
            sep.push_bind(net);
        }
        Value::Ipv6(a) => {
            let net = sqlx::types::ipnetwork::IpNetwork::new(std::net::IpAddr::V6(*a), 128)
                .expect("/128 prefix always valid for any IPv6 address");
            sep.push_bind(net);
        }
        Value::Json(j) => {
            sep.push_bind(j.clone());
        }
        Value::BigInt(b) => {
            // Lift BigInt to BigDecimal scale 0 — sqlx pg numeric only encodes via BigDecimal.
            sep.push_bind(BigDecimal::new(b.clone(), 0));
        }
        Value::Decimal(d) => {
            sep.push_bind(d.clone());
        }
        Value::Object(_) => {
            // Convert the structured document into a serde_json::Value
            // for JSON/JSONB binding. Validation guarantees only
            // compatible types reach the sink, so encode should not fail.
            let j = air_elt_core::types::json_encode::value_to_json(v)
                .expect("Value::Object json encode must not fail after validation");
            sep.push_bind(j);
        }
        Value::UInt8(_) | Value::UInt16(_) | Value::UInt32(_) | Value::UInt64(_) => {
            unreachable!(
                "unsigned values cannot reach a postgres sink: pg schemas never \
                 produce UInt* (no native unsigned int columns), and the convert \
                 dispatcher rewrites every UInt → Int*/BigInt/Decimal mapping into \
                 the target variant before binding"
            )
        }
        Value::Interval(_) => {
            unreachable!(
                "Value::Interval cannot reach a postgres sink: Interval is a \
                 redis-only canonical type and the matrix rejects every \
                 `* → Interval` pair except identity into a redis column"
            )
        }
        Value::Custom(v) => {
            // HLL: bind raw bytes, then append `::hll` to cast the
            // bound bytea to the extension type at execution time.
            // sqlx has no native HLL type registration, so the cast
            // is mandatory — without it, PG rejects the binary as
            // `bytea` rather than `hll`.
            if let Some(hll) = v.as_any().downcast_ref::<PgHllValue>() {
                sep.push_bind(hll.0.clone());
                sep.push_unseparated("::hll");
            } else if let Some(inet) = v.as_any().downcast_ref::<PgInetValue>() {
                // Bind the IpNetwork (host or CIDR) — sqlx-postgres
                // routes it to the `inet` OID directly.
                sep.push_bind(inet.0);
            } else {
                unreachable!(
                    "postgres sink received unsupported custom value kind {:?}",
                    {
                        let dt = v.dyn_type();
                        dt.kind().to_string()
                    }
                )
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_core::error::JsonEncodeError;
    use air_elt_core::types::convert::ConvertError;
    use air_elt_core::types::convert::context::ConversionContext;
    use air_elt_core::types::dynamic::{DynType, DynValue};
    use sqlx::QueryBuilder;
    use std::any::Any;

    /// Stub Custom type that is NOT the pg-only `postgresql.hll`
    /// exception — used to verify the debug_assert trips.
    #[derive(Debug)]
    struct StubType;

    impl DynType for StubType {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.unknown_custom"
        }
        fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unreachable!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unreachable!()
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
    }

    #[derive(Debug)]
    struct StubValue;

    impl DynValue for StubValue {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
        fn is_equal(&self, _other: &dyn DynValue) -> bool {
            false
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(StubValue)
        }
        fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
            Ok(serde_json::Value::Null)
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "SQL sink received unexpected Value::Custom")]
    fn debug_assert_trips_on_unknown_custom() {
        let mut qb: QueryBuilder<'_, sqlx::Postgres> = QueryBuilder::new("SELECT ");
        let mut sep = qb.separated(", ");
        let v = Value::Custom(Box::new(StubValue));
        let dt = DataType::Custom(Box::new(StubType));
        bind_value_separated(&mut sep, &v, &dt);
    }
}
