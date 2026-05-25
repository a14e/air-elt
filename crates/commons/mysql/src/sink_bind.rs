//! MySQL sink-side `Separated` binding.
//!
//! Mirror of `commons-pg::sink_bind` for the mysql sink. Used by both
//! the insert path (`push_values`) and the DELETE path (`push_tuples`
//! / single-key `separated(", ")`). Keep the arms in lockstep with
//! `null_bind::bind_typed_null` (which targets a `Query`).

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::MySql;
use sqlx::query_builder::Separated;

use air_elt_core::types::{DataType, Value};

pub fn bind_value_separated(sep: &mut Separated<'_, '_, MySql, &str>, v: &Value, dt: &DataType) {
    // Fraud-detector (anti-shortcut #6): SQL sinks can only consume
    // canonical `Value::*` variants (incl. `Value::Json`). Custom row
    // payloads (e.g. `mongodb.bson_object`) must be matrix-converted to
    // `Value::Json` BEFORE reaching bind. MySQL has no Custom
    // exceptions today (no `mysql.*` Custom types). Any Custom value
    // reaching this function is a missing matrix conversion and would
    // otherwise fall through to `unreachable!()` at runtime. Trip
    // loudly in debug builds; release behaviour is unchanged.
    #[cfg(debug_assertions)]
    {
        if let Value::Custom(c) = v {
            panic!(
                "SQL sink received unexpected Value::Custom(kind={}); matrix conversion to Json missing",
                {
                    let dt = c.dyn_type();
                    dt.kind().to_string()
                }
            );
        }
    }
    match v {
        Value::Null => match dt {
            DataType::Bool => {
                sep.push_bind::<Option<bool>>(None);
            }
            DataType::Int8 => {
                sep.push_bind::<Option<i8>>(None);
            }
            DataType::Int16 => {
                sep.push_bind::<Option<i16>>(None);
            }
            DataType::Int32 => {
                sep.push_bind::<Option<i32>>(None);
            }
            DataType::Int64 => {
                sep.push_bind::<Option<i64>>(None);
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
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Ipv4 | DataType::Ipv6 => {
                // MySQL has no native IP type. Operators store IPs as
                // VARCHAR/TEXT; canonical IPv4/IPv6 binds as a typed
                // NULL string.
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Json | DataType::Object => {
                sep.push_bind::<Option<serde_json::Value>>(None);
            }
            DataType::BigInt { .. } | DataType::Decimal { .. } => {
                sep.push_bind::<Option<BigDecimal>>(None);
            }
            DataType::UInt8 => {
                sep.push_bind::<Option<u8>>(None);
            }
            DataType::UInt16 => {
                sep.push_bind::<Option<u16>>(None);
            }
            DataType::UInt32 => {
                sep.push_bind::<Option<u32>>(None);
            }
            DataType::UInt64 => {
                sep.push_bind::<Option<u64>>(None);
            }
            DataType::Xml => {
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Union(_) => unreachable!("mysql sinks never carry Union types"),
            DataType::Custom(_) => unreachable!(
                "DataType::Custom must be handled by the connector before reaching sink_bind"
            ),
        },
        Value::Bool(b) => {
            sep.push_bind(*b);
        }
        Value::Int8(n) => {
            sep.push_bind(*n);
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
            sep.push_bind(u.to_string());
        }
        Value::Ipv4(a) => {
            // VARCHAR-stored IPs: bind the canonical text form. Works
            // for VARCHAR(15)/VARCHAR(45) and TEXT columns alike, and
            // aligns with MariaDB's IS_IPV4()/IS_IPV6() helpers which
            // expect strings.
            sep.push_bind(a.to_string());
        }
        Value::Ipv6(a) => {
            sep.push_bind(a.to_string());
        }
        Value::Json(j) => {
            sep.push_bind(j.clone());
        }
        Value::BigInt(b) => {
            sep.push_bind(BigDecimal::new(b.clone(), 0));
        }
        Value::Decimal(d) => {
            sep.push_bind(d.clone());
        }
        Value::UInt8(n) => {
            sep.push_bind(*n);
        }
        Value::UInt16(n) => {
            sep.push_bind(*n);
        }
        Value::UInt32(n) => {
            sep.push_bind(*n);
        }
        Value::UInt64(n) => {
            sep.push_bind(*n);
        }
        Value::Object(_) => {
            // Convert the structured document into a serde_json::Value
            // for JSON binding. Validation guarantees compatible types.
            let j = air_elt_core::types::json_encode::value_to_json(v)
                .expect("Value::Object json encode must not fail after validation");
            sep.push_bind(j);
        }
        Value::Custom(_) => {
            unreachable!("Value::Custom must be handled by the connector before reaching sink_bind")
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

    /// Stub Custom type — MySQL has no Custom exceptions, so any
    /// `Value::Custom` reaching the bind path must trip the
    /// debug_assert.
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
        fn eq_dyn(&self, _other: &dyn DynValue) -> bool {
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
    fn debug_assert_trips_on_any_custom() {
        let mut qb: QueryBuilder<'_, sqlx::MySql> = QueryBuilder::new("SELECT ");
        let mut sep = qb.separated(", ");
        let v = Value::Custom(Box::new(StubValue));
        let dt = DataType::Custom(Box::new(StubType));
        bind_value_separated(&mut sep, &v, &dt);
    }
}
