use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Deserializer, Serialize};
use std::str::FromStr;
use uuid::Uuid;

use crate::types::dynamic::DynValue;

/// Serialised with a `{ "type": "...", "value": ... }` internal tag so that
/// round-tripping through JSONB (cursor storage) preserves the exact variant.
/// Untagged serde would silently coerce e.g. `Int64(42)` → `Int16(42)`.
#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),
    Float32(f32),
    Float64(f64),
    /// Arbitrary-precision integer. Carries a `num_bigint::BigInt` directly,
    /// not a `BigDecimal`, so plain integer pipelines avoid mantissa+scale
    /// arithmetic. Cursor JSON locks to the canonical decimal-string form
    /// (num-bigint's default emits a `[sign, [u32 digits]]` tuple, which
    /// is lossless but unreadable and brittle across versions).
    BigInt(BigInt),
    /// Arbitrary-precision decimal. JSON cursor storage round-trips through
    /// the canonical decimal-string form (BigDecimal's default serde repr is
    /// a JSON number, which f64-truncates; we lock to string instead).
    Decimal(BigDecimal),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Timestamp(DateTime<Utc>),
    Uuid(Uuid),
    Json(serde_json::Value),
    /// Connector-specific opaque value. Cursor JSON storage MUST NOT see
    /// this variant — Serialize/Deserialize error on it deliberately;
    /// validation rejects flows that would persist a Custom cursor.
    Custom(Box<dyn DynValue>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int16(n) => Value::Int16(*n),
            Value::Int32(n) => Value::Int32(*n),
            Value::Int64(n) => Value::Int64(*n),
            Value::UInt8(n) => Value::UInt8(*n),
            Value::UInt16(n) => Value::UInt16(*n),
            Value::UInt32(n) => Value::UInt32(*n),
            Value::UInt64(n) => Value::UInt64(*n),
            Value::Float32(n) => Value::Float32(*n),
            Value::Float64(n) => Value::Float64(*n),
            Value::BigInt(b) => Value::BigInt(b.clone()),
            Value::Decimal(d) => Value::Decimal(d.clone()),
            Value::Text(s) => Value::Text(s.clone()),
            Value::Bytes(b) => Value::Bytes(b.clone()),
            Value::Date(d) => Value::Date(*d),
            Value::Timestamp(t) => Value::Timestamp(*t),
            Value::Uuid(u) => Value::Uuid(*u),
            Value::Json(j) => Value::Json(j.clone()),
            Value::Custom(v) => Value::Custom((**v).clone_box()),
        }
    }
}

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        use Value::*;
        match (self, other) {
            (Null, Null) => true,
            (Bool(a), Bool(b)) => a == b,
            (Int16(a), Int16(b)) => a == b,
            (Int32(a), Int32(b)) => a == b,
            (Int64(a), Int64(b)) => a == b,
            (UInt8(a), UInt8(b)) => a == b,
            (UInt16(a), UInt16(b)) => a == b,
            (UInt32(a), UInt32(b)) => a == b,
            (UInt64(a), UInt64(b)) => a == b,
            (Float32(a), Float32(b)) => a == b,
            (Float64(a), Float64(b)) => a == b,
            (BigInt(a), BigInt(b)) => a == b,
            (Decimal(a), Decimal(b)) => a == b,
            (Text(a), Text(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            (Date(a), Date(b)) => a == b,
            (Timestamp(a), Timestamp(b)) => a == b,
            (Uuid(a), Uuid(b)) => a == b,
            (Json(a), Json(b)) => a == b,
            (Custom(a), Custom(b)) => (**a).eq_dyn(&**b),
            _ => false,
        }
    }
}

// ---- Hand-rolled Serialize/Deserialize ---------------------------------
//
// The original derive used:
//   `#[serde(tag = "type", content = "value", rename_all = "snake_case")]`
// We mirror that wire format byte-for-byte so cursor JSON storage stays
// readable regardless of when a row was written:
//   - unit variants: `{"type":"null"}` (no `value` key — serde drops it
//     for unit variants under internally-tagged enums).
//   - tuple variants with one inner: `{"type":"int32","value":42}`.
// `BigInt` and `Decimal` use the canonical decimal-string form (see
// the wrappers below).
//
// `Custom` errors on both Serialize and Deserialize. Validation guards
// against cursor JSON ever seeing it.

impl Serialize for Value {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Value::Null => {
                let mut m = serializer.serialize_map(Some(1))?;
                m.serialize_entry("type", "null")?;
                m.end()
            }
            Value::Bool(b) => emit(serializer, "bool", b),
            Value::Int16(n) => emit(serializer, "int16", n),
            Value::Int32(n) => emit(serializer, "int32", n),
            Value::Int64(n) => emit(serializer, "int64", n),
            Value::UInt8(n) => emit(serializer, "u_int8", n),
            Value::UInt16(n) => emit(serializer, "u_int16", n),
            Value::UInt32(n) => emit(serializer, "u_int32", n),
            Value::UInt64(n) => emit(serializer, "u_int64", n),
            Value::Float32(n) => emit(serializer, "float32", n),
            Value::Float64(n) => emit(serializer, "float64", n),
            Value::BigInt(b) => emit(serializer, "big_int", &b.to_str_radix(10)),
            Value::Decimal(d) => emit(serializer, "decimal", &d.to_string()),
            Value::Text(s) => emit(serializer, "text", s),
            Value::Bytes(b) => emit(serializer, "bytes", b),
            Value::Date(d) => emit(serializer, "date", d),
            Value::Timestamp(t) => emit(serializer, "timestamp", t),
            Value::Uuid(u) => emit(serializer, "uuid", u),
            Value::Json(j) => emit(serializer, "json", j),
            Value::Custom(_) => Err(serde::ser::Error::custom(
                "Value::Custom cannot be serialized — \
                 connector-specific values must not be persisted to cursor JSON; \
                 validation::assemble guards against this",
            )),
        }
    }
}

fn emit<S, T>(serializer: S, tag: &str, value: &T) -> Result<S::Ok, S::Error>
where
    S: Serializer,
    T: Serialize + ?Sized,
{
    let mut m = serializer.serialize_map(Some(2))?;
    m.serialize_entry("type", tag)?;
    m.serialize_entry("value", value)?;
    m.end()
}

impl<'de> Deserialize<'de> for Value {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(ValueVisitor)
    }
}

struct ValueVisitor;

impl<'de> Visitor<'de> for ValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("an internally-tagged Value map")
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        // The original `#[serde(tag = "type", content = "value")]` shape
        // requires the `type` key first or anywhere; serde's own visitor
        // tolerates either order. We do the same: drain into a small
        // buffer then dispatch.
        let mut tag: Option<String> = None;
        let mut raw_value: Option<serde_json::Value> = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "type" => {
                    if tag.is_some() {
                        return Err(de::Error::duplicate_field("type"));
                    }
                    tag = Some(map.next_value()?);
                }
                "value" => {
                    if raw_value.is_some() {
                        return Err(de::Error::duplicate_field("value"));
                    }
                    raw_value = Some(map.next_value()?);
                }
                other => return Err(de::Error::unknown_field(other, &["type", "value"])),
            }
        }
        let tag = tag.ok_or_else(|| de::Error::missing_field("type"))?;
        // For each variant we synthesise the original payload from
        // `raw_value`. Unit variants ignore `value` entirely (matches
        // serde's behaviour: it accepts the field's absence).
        let v = raw_value.unwrap_or(serde_json::Value::Null);
        match tag.as_str() {
            "null" => Ok(Value::Null),
            "bool" => decode(v).map(Value::Bool),
            "int16" => decode(v).map(Value::Int16),
            "int32" => decode(v).map(Value::Int32),
            "int64" => decode(v).map(Value::Int64),
            "u_int8" => decode(v).map(Value::UInt8),
            "u_int16" => decode(v).map(Value::UInt16),
            "u_int32" => decode(v).map(Value::UInt32),
            "u_int64" => decode(v).map(Value::UInt64),
            "float32" => decode(v).map(Value::Float32),
            "float64" => decode(v).map(Value::Float64),
            "big_int" => {
                let s: String = decode(v)?;
                BigInt::from_str(&s)
                    .map(Value::BigInt)
                    .map_err(de::Error::custom)
            }
            "decimal" => {
                let s: String = decode(v)?;
                BigDecimal::from_str(&s)
                    .map(Value::Decimal)
                    .map_err(de::Error::custom)
            }
            "text" => decode(v).map(Value::Text),
            "bytes" => decode(v).map(Value::Bytes),
            "date" => decode(v).map(Value::Date),
            "timestamp" => decode(v).map(Value::Timestamp),
            "uuid" => decode(v).map(Value::Uuid),
            "json" => Ok(Value::Json(v)),
            "custom" => Err(de::Error::custom(
                "Value::Custom cannot be deserialized — \
                 connector-specific values have no global registry",
            )),
            other => Err(de::Error::unknown_variant(
                other,
                &[
                    "null",
                    "bool",
                    "int16",
                    "int32",
                    "int64",
                    "u_int8",
                    "u_int16",
                    "u_int32",
                    "u_int64",
                    "float32",
                    "float64",
                    "big_int",
                    "decimal",
                    "text",
                    "bytes",
                    "date",
                    "timestamp",
                    "uuid",
                    "json",
                ],
            )),
        }
    }
}

fn decode<T, E>(v: serde_json::Value) -> Result<T, E>
where
    T: for<'de> Deserialize<'de>,
    E: de::Error,
{
    serde_json::from_value(v).map_err(de::Error::custom)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::any::Any;

    #[test]
    fn null_round_trips() {
        let v = serde_json::to_value(Value::Null).unwrap();
        assert_eq!(v, serde_json::json!({"type": "null"}));
        let back: Value = serde_json::from_value(v).unwrap();
        assert_eq!(back, Value::Null);
    }

    /// Regression: every non-Custom variant emits the same wire format
    /// the prior `#[serde(tag="type", content="value", rename_all="snake_case")]`
    /// derive produced. Cursor JSON storage already relies on this shape;
    /// future drift breaks the test.
    #[test]
    fn serde_binary_compat_per_variant() {
        let cases: Vec<(Value, serde_json::Value)> = vec![
            (
                Value::Bool(true),
                serde_json::json!({"type":"bool","value":true}),
            ),
            (
                Value::Int16(7),
                serde_json::json!({"type":"int16","value":7}),
            ),
            (
                Value::Int32(7),
                serde_json::json!({"type":"int32","value":7}),
            ),
            (
                Value::Int64(7),
                serde_json::json!({"type":"int64","value":7}),
            ),
            (
                Value::UInt8(7),
                serde_json::json!({"type":"u_int8","value":7}),
            ),
            (
                Value::UInt16(7),
                serde_json::json!({"type":"u_int16","value":7}),
            ),
            (
                Value::UInt32(7),
                serde_json::json!({"type":"u_int32","value":7}),
            ),
            (
                Value::UInt64(7),
                serde_json::json!({"type":"u_int64","value":7}),
            ),
            (
                Value::Float32(1.5),
                serde_json::json!({"type":"float32","value":1.5}),
            ),
            (
                Value::Float64(1.5),
                serde_json::json!({"type":"float64","value":1.5}),
            ),
            (
                Value::BigInt(BigInt::from(42)),
                serde_json::json!({"type":"big_int","value":"42"}),
            ),
            (
                Value::Decimal("12.34".parse().unwrap()),
                serde_json::json!({"type":"decimal","value":"12.34"}),
            ),
            (
                Value::Text("hi".into()),
                serde_json::json!({"type":"text","value":"hi"}),
            ),
            (
                Value::Bytes(vec![1, 2, 3]),
                serde_json::json!({"type":"bytes","value":[1,2,3]}),
            ),
            (
                Value::Date("2024-01-15".parse().unwrap()),
                serde_json::json!({"type":"date","value":"2024-01-15"}),
            ),
            (
                Value::Timestamp("2024-01-15T12:00:00Z".parse().unwrap()),
                serde_json::json!({"type":"timestamp","value":"2024-01-15T12:00:00Z"}),
            ),
            (
                Value::Uuid(uuid::Uuid::nil()),
                serde_json::json!({"type":"uuid","value":"00000000-0000-0000-0000-000000000000"}),
            ),
            (
                Value::Json(serde_json::json!({"k":1})),
                serde_json::json!({"type":"json","value":{"k":1}}),
            ),
        ];
        for (variant, expected) in cases {
            let got = serde_json::to_value(variant.clone()).unwrap();
            assert_eq!(got, expected, "serialise mismatch for {variant:?}");
            let back: Value = serde_json::from_value(expected).unwrap();
            assert_eq!(back, variant, "round-trip mismatch for {variant:?}");
        }
    }

    /// Test stand-in `DynValue` so we can construct a `Value::Custom` and
    /// verify the serialize-error guard.
    #[derive(Debug)]
    struct StubValue;

    impl DynValue for StubValue {
        fn dyn_type(&self) -> Box<dyn crate::types::dynamic::DynType> {
            unimplemented!()
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
        fn eq_dyn(&self, _other: &dyn DynValue) -> bool {
            true
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(StubValue)
        }
    }

    #[test]
    fn custom_serialize_errors() {
        let v = Value::Custom(Box::new(StubValue));
        let res = serde_json::to_value(&v);
        assert!(res.is_err(), "Value::Custom must not serialise");
    }

    #[test]
    fn custom_clone_round_trips_via_clone_box() {
        let original = Value::Custom(Box::new(StubValue));
        let cloned = original.clone();
        // `StubValue::eq_dyn` returns true unconditionally — the
        // intent here is to exercise the `Value::Clone` arm and
        // confirm `PartialEq` reaches `eq_dyn` on the boxed payload.
        assert_eq!(original, cloned);
    }
}
