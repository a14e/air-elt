use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use serde::de::{self, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Deserializer, Serialize};
use std::str::FromStr;
use uuid::Uuid;

use crate::dynamic::DynValue;

/// Serialised with a `{ "type": "...", "value": ... }` internal tag so that
/// round-tripping through JSONB (cursor storage) preserves the exact variant.
/// Untagged serde would silently coerce e.g. `Int64(42)` → `Int16(42)`.
///
/// `Value::Custom` is admitted only when the underlying [`DynType`]
/// declares [`cursor_compatible() == true`](crate::dynamic::DynType::cursor_compatible).
/// In that case the wire shape is
/// `{ "type": "custom", "kind": "<kind>", "value": <json> }`, with
/// `<json>` produced by [`DynValue::to_json`]. The bare
/// `Value::Deserialize` impl does NOT decode `custom` envelopes —
/// recovering a `Box<dyn DynValue>` needs the expected descriptor
/// up front. The typed entry point is
/// [`crate::DataType::decode_cursor_json`], driven by the
/// storage layer from the source-schema cursor types.
/// Cursor-incompatible custom values error out on `Serialize` —
/// validation rejects them up front, but the serializer keeps the
/// safety net.
#[derive(Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Int8(i8),
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
    Ipv4(std::net::Ipv4Addr),
    Ipv6(std::net::Ipv6Addr),
    Json(serde_json::Value),
    /// Ordered key-value document. Keys are always strings, values are
    /// heterogeneous (any `Value` variant). Order matters for
    /// deterministic serialisation.
    Object(Vec<(String, Value)>),
    /// Connector-specific opaque value. Cursor JSON storage admits
    /// this variant iff the underlying `DynType::cursor_compatible()`
    /// returns `true` AND the matching descriptor overrides
    /// `DynType::decode_cursor_value` (the typed reload entry, called
    /// from [`crate::DataType::decode_cursor_json`]).
    /// `Serialize` errors out for cursor-incompatible custom values;
    /// the bare `Value::Deserialize` always errors on `custom`
    /// envelopes regardless — typed decode via `DataType` is the only
    /// supported reload path.
    Custom(Box<dyn DynValue>),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }

    /// Map this `Value` variant onto its concrete [`crate::DataType`].
    /// Returns `None` for `Value::Null` (carries no type information by
    /// design — nullability is on `Field`, not `DataType`).
    ///
    /// Width-bearing variants (`Text`, `Bytes`, `BigInt`, `Decimal`)
    /// report unbounded because the value carries no width metadata;
    /// callers that need the post-conversion width must read it off the
    /// **target** type. Used by the dynamic-source path of
    /// `TransformOp::Convert` (when `ColumnConversionPlan.source = None`)
    /// for schemaless sources, and by the `Union` source-side runtime
    /// re-dispatch inside the convert dispatcher.
    pub fn data_type(&self) -> Option<crate::DataType> {
        use crate::DataType;
        match self {
            Value::Null => None,
            Value::Bool(_) => Some(DataType::Bool),
            Value::Int8(_) => Some(DataType::Int8),
            Value::Int16(_) => Some(DataType::Int16),
            Value::Int32(_) => Some(DataType::Int32),
            Value::Int64(_) => Some(DataType::Int64),
            Value::UInt8(_) => Some(DataType::UInt8),
            Value::UInt16(_) => Some(DataType::UInt16),
            Value::UInt32(_) => Some(DataType::UInt32),
            Value::UInt64(_) => Some(DataType::UInt64),
            Value::Float32(_) => Some(DataType::Float32),
            Value::Float64(_) => Some(DataType::Float64),
            Value::BigInt(_) => Some(DataType::BigInt { width: None }),
            Value::Decimal(_) => Some(DataType::Decimal {
                precision: None,
                scale: None,
            }),
            Value::Text(_) => Some(DataType::Text { size: None }),
            Value::Bytes(_) => Some(DataType::Bytes { size: None }),
            Value::Date(_) => Some(DataType::Date),
            Value::Timestamp(_) => Some(DataType::Timestamp),
            Value::Uuid(_) => Some(DataType::Uuid),
            Value::Ipv4(_) => Some(DataType::Ipv4),
            Value::Ipv6(_) => Some(DataType::Ipv6),
            Value::Json(_) => Some(DataType::Json),
            Value::Object(_) => Some(DataType::Object),
            Value::Custom(v) => Some(DataType::Custom(v.dyn_type())),
        }
    }
}

impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Null => Value::Null,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int8(n) => Value::Int8(*n),
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
            Value::Ipv4(a) => Value::Ipv4(*a),
            Value::Ipv6(a) => Value::Ipv6(*a),
            Value::Json(j) => Value::Json(j.clone()),
            Value::Object(entries) => Value::Object(entries.clone()),
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
            (Int8(a), Int8(b)) => a == b,
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
            (Ipv4(a), Ipv4(b)) => a == b,
            (Ipv6(a), Ipv6(b)) => a == b,
            (Json(a), Json(b)) => a == b,
            (Object(a), Object(b)) => a == b,
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
// `Custom` participates in the wire format when the underlying
// `DynType::cursor_compatible()` is `true`. Wire shape:
//   {"type":"custom","kind":"<kind>","value":<json from to_json()>}
// The bare `Value::Deserialize` impl refuses `custom` envelopes —
// typed reload lives on `DataType::decode_cursor_json`. Cursor-
// incompatible kinds also error out on `Serialize` (validation
// already rejects them up front, but the serializer keeps the
// safety net).

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
            Value::Int8(n) => emit(serializer, "int8", n),
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
            Value::Ipv4(a) => emit(serializer, "ipv4", &a.to_string()),
            Value::Ipv6(a) => emit(serializer, "ipv6", &a.to_string()),
            Value::Json(j) => emit(serializer, "json", j),
            Value::Object(entries) => {
                let json_map: serde_json::Map<String, serde_json::Value> = entries
                    .iter()
                    .map(|(k, v)| {
                        let json_v =
                            crate::json_encode::value_to_json(v).unwrap_or(serde_json::Value::Null);
                        (k.clone(), json_v)
                    })
                    .collect();
                emit(serializer, "object", &serde_json::Value::Object(json_map))
            }
            Value::Custom(inner) => {
                let dt = inner.dyn_type();
                if !dt.cursor_compatible() {
                    return Err(serde::ser::Error::custom(format!(
                        "Value::Custom (kind = {:?}) is not cursor_compatible; \
                         validation::assemble rejects flows whose cursor would \
                         carry this type",
                        dt.kind()
                    )));
                }
                let payload = inner.to_json().map_err(|e| {
                    serde::ser::Error::custom(format!(
                        "Value::Custom (kind = {:?}) to_json failed: {e}",
                        dt.kind()
                    ))
                })?;
                let mut m = serializer.serialize_map(Some(3))?;
                m.serialize_entry("type", "custom")?;
                m.serialize_entry("kind", dt.kind())?;
                m.serialize_entry("value", &payload)?;
                m.end()
            }
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
        // buffer then dispatch. The `kind` slot is only used by the
        // `"custom"` variant (cursor-codec discriminator).
        let mut tag: Option<String> = None;
        let mut raw_value: Option<serde_json::Value> = None;
        let mut kind: Option<String> = None;
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
                "kind" => {
                    if kind.is_some() {
                        return Err(de::Error::duplicate_field("kind"));
                    }
                    kind = Some(map.next_value()?);
                }
                other => {
                    return Err(de::Error::unknown_field(other, &["type", "value", "kind"]));
                }
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
            "int8" => decode(v).map(Value::Int8),
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
            "ipv4" => {
                let s: String = decode(v)?;
                std::net::Ipv4Addr::from_str(&s)
                    .map(Value::Ipv4)
                    .map_err(de::Error::custom)
            }
            "ipv6" => {
                let s: String = decode(v)?;
                std::net::Ipv6Addr::from_str(&s)
                    .map(Value::Ipv6)
                    .map_err(de::Error::custom)
            }
            "json" => Ok(Value::Json(v)),
            "object" => {
                let map = v
                    .as_object()
                    .ok_or_else(|| de::Error::custom("object value must be a JSON object"))?;
                let entries: Vec<(String, Value)> = map
                    .iter()
                    .map(|(k, val)| Ok((k.clone(), Value::Json(val.clone()))))
                    .collect::<Result<_, A::Error>>()?;
                Ok(Value::Object(entries))
            }
            "custom" => {
                // Custom values can't deserialize through the bare
                // `Value` path: a `Box<dyn DynValue>` needs the
                // descriptor's `decode_cursor_value` impl, which we
                // can only reach when the caller supplies the
                // expected `DataType::Custom(t)` ahead of time. Use
                // [`crate::DataType::decode_cursor_json`]
                // instead — the storage layer does so per cursor
                // field, looking the expected type up on the source
                // schema.
                let _ = kind;
                Err(de::Error::custom(
                    "Value::Deserialize does not handle `custom` envelopes — \
                     callers must dispatch through DataType::decode_cursor_json \
                     with the expected source-schema type",
                ))
            }
            other => Err(de::Error::unknown_variant(
                other,
                &[
                    "null",
                    "bool",
                    "int8",
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
                    "ipv4",
                    "ipv6",
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
            (Value::Int8(7), serde_json::json!({"type":"int8","value":7})),
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
                Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 1)),
                serde_json::json!({"type":"ipv4","value":"192.0.2.1"}),
            ),
            (
                Value::Ipv6("2001:db8::1".parse().unwrap()),
                serde_json::json!({"type":"ipv6","value":"2001:db8::1"}),
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

    /// Test stand-in `DynType` paired with `StubValue` to exercise
    /// the cursor-incompatible Serialize guard.
    #[derive(Debug, Clone, Copy)]
    struct StubType;

    impl crate::dynamic::DynType for StubType {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn kind(&self) -> &str {
            "test.stub"
        }
        fn can_convert_to(&self, _t: &crate::DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &crate::DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &crate::DataType,
            _ctx: &crate::convert::context::ConversionContext,
        ) -> Result<Value, crate::convert::ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &crate::DataType,
            _ctx: &crate::convert::context::ConversionContext,
        ) -> Result<Value, crate::convert::ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn crate::dynamic::DynType> {
            Box::new(*self)
        }
    }

    /// Test stand-in `DynValue` so we can construct a `Value::Custom` and
    /// verify the serialize-error guard.
    #[derive(Debug)]
    struct StubValue;

    impl DynValue for StubValue {
        fn dyn_type(&self) -> Box<dyn crate::dynamic::DynType> {
            Box::new(StubType)
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

    /// Cursor-compatible stub that participates in the round-trip
    /// test. Pairs a `DynType` whose `cursor_compatible() == true`
    /// with a `DynValue` whose `to_json` emits a plain number.
    #[derive(Debug, Clone, Copy)]
    struct CursorStubType;

    impl crate::dynamic::DynType for CursorStubType {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn kind(&self) -> &str {
            "test.cursor_stub"
        }
        fn cursor_compatible(&self) -> bool {
            true
        }
        fn can_convert_to(&self, _t: &crate::DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &crate::DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &crate::DataType,
            _ctx: &crate::convert::context::ConversionContext,
        ) -> Result<Value, crate::convert::ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &crate::DataType,
            _ctx: &crate::convert::context::ConversionContext,
        ) -> Result<Value, crate::convert::ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn crate::dynamic::DynType> {
            Box::new(*self)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CursorStubValue(u32);

    impl DynValue for CursorStubValue {
        fn dyn_type(&self) -> Box<dyn crate::dynamic::DynType> {
            Box::new(CursorStubType)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
        fn eq_dyn(&self, other: &dyn DynValue) -> bool {
            other
                .as_any()
                .downcast_ref::<CursorStubValue>()
                .is_some_and(|o| o.0 == self.0)
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(self.clone())
        }
        fn to_json(&self) -> Result<serde_json::Value, crate::error::JsonEncodeError> {
            Ok(serde_json::Value::from(self.0))
        }
    }

    #[test]
    fn custom_serialize_errors_for_non_cursor_compatible() {
        // `StubType::cursor_compatible()` defaults to false → the
        // Serialize guard refuses with a clear message that traces
        // the kind. (Validation rejects flows before they get here,
        // but the serializer keeps the safety net.)
        let v = Value::Custom(Box::new(StubValue));
        let res = serde_json::to_value(&v);
        assert!(
            res.is_err(),
            "non-cursor-compatible Value::Custom must not serialise"
        );
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

    /// Custom values serialise into the `{type,kind,value}` envelope
    /// for cursor JSON storage, but the bare `Value` deserialize path
    /// no longer attempts to recover them: a `Box<dyn DynValue>` needs
    /// the expected descriptor up front. The typed entry point is
    /// `DataType::decode_cursor_json` on the storage / runner side.
    #[test]
    fn cursor_compatible_custom_serialize_via_envelope() {
        let v = Value::Custom(Box::new(CursorStubValue(42)));
        let got = serde_json::to_value(v.clone()).expect("serialize");
        assert_eq!(
            got,
            serde_json::json!({
                "type": "custom",
                "kind": "test.cursor_stub",
                "value": 42,
            })
        );
    }

    #[test]
    fn custom_deserialize_through_value_serde_errors() {
        // The bare `Value` Deserialize path refuses `custom` envelopes
        // — typed decode is the only supported entry. The storage
        // layer dispatches via `DataType::decode_cursor_json` instead.
        let raw = serde_json::json!({
            "type": "custom",
            "kind": "test.cursor_stub",
            "value": 1,
        });
        let res: Result<Value, _> = serde_json::from_value(raw);
        assert!(
            res.is_err(),
            "bare Value::Deserialize must refuse `custom` envelopes"
        );
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Strategy that yields any non-`Custom` `Value`, including
    /// width-bearing carriers (`BigInt`, `Decimal`, `Text`, `Bytes`)
    /// and the temporal / network variants that lack a derived
    /// `Arbitrary`. NaN floats are filtered out at the strategy
    /// level so downstream property tests don't each repeat the
    /// `prop_assume!` preamble — `f.is_nan() != f.is_nan()` would
    /// break every reflexivity / round-trip assertion.
    fn any_non_custom_value() -> impl Strategy<Value = Value> {
        let big_int_strategy =
            any::<i128>().prop_map(|n| Value::BigInt(num_bigint::BigInt::from(n)));
        let decimal_strategy = (any::<i64>(), 0i64..18).prop_map(|(mantissa, scale)| {
            Value::Decimal(bigdecimal::BigDecimal::new(
                num_bigint::BigInt::from(mantissa),
                scale,
            ))
        });
        let date_strategy = (1970i32..2100, 1u32..=12, 1u32..=28)
            .prop_map(|(y, m, d)| Value::Date(chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()));
        let timestamp_strategy = any::<i64>().prop_filter_map("range", |seconds| {
            let s = seconds % 4_000_000_000;
            chrono::DateTime::<chrono::Utc>::from_timestamp(s, 0).map(Value::Timestamp)
        });
        let uuid_strategy = any::<[u8; 16]>().prop_map(|b| Value::Uuid(uuid::Uuid::from_bytes(b)));
        let ipv4_strategy =
            any::<u32>().prop_map(|n| Value::Ipv4(std::net::Ipv4Addr::from(n.to_be_bytes())));
        let ipv6_strategy =
            any::<[u8; 16]>().prop_map(|b| Value::Ipv6(std::net::Ipv6Addr::from(b)));
        let json_strategy = any::<i64>().prop_map(|n| Value::Json(serde_json::json!({ "n": n })));
        let float32_strategy = any::<f32>()
            .prop_filter("no NaN", |f| !f.is_nan())
            .prop_map(Value::Float32);
        let float64_strategy = any::<f64>()
            .prop_filter("no NaN", |f| !f.is_nan())
            .prop_map(Value::Float64);

        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i8>().prop_map(Value::Int8),
            any::<i16>().prop_map(Value::Int16),
            any::<i32>().prop_map(Value::Int32),
            any::<i64>().prop_map(Value::Int64),
            any::<u8>().prop_map(Value::UInt8),
            any::<u16>().prop_map(Value::UInt16),
            any::<u32>().prop_map(Value::UInt32),
            any::<u64>().prop_map(Value::UInt64),
            float32_strategy,
            float64_strategy,
            big_int_strategy,
            decimal_strategy,
            ".*".prop_map(Value::Text),
            prop::collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
            date_strategy,
            timestamp_strategy,
            uuid_strategy,
            ipv4_strategy,
            ipv6_strategy,
            json_strategy,
        ]
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn value_clone_eq_reflexive(#[strategy(any_non_custom_value())] v: Value) {
        let cloned = v.clone();
        prop_assert_eq!(&cloned, &v);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn value_serde_round_trip_non_custom(#[strategy(any_non_custom_value())] v: Value) {
        let json = serde_json::to_value(&v).expect("serialize");
        let back: Value = serde_json::from_value(json).expect("deserialize");
        prop_assert_eq!(back, v);
    }

    /// Property: `Value::data_type()` returns `None` for `Null` and
    /// `Some(t)` matching the variant's canonical type for every other
    /// non-Custom variant. Folds together what would otherwise be a
    /// per-variant table — the variant→type mapping is the property,
    /// not the individual cells.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn data_type_reports_canonical_for_non_custom(#[strategy(any_non_custom_value())] v: Value) {
        use crate::DataType;
        let got = v.data_type();
        match &v {
            Value::Null => prop_assert!(got.is_none()),
            Value::Bool(_) => prop_assert_eq!(got, Some(DataType::Bool)),
            Value::Int8(_) => prop_assert_eq!(got, Some(DataType::Int8)),
            Value::Int16(_) => prop_assert_eq!(got, Some(DataType::Int16)),
            Value::Int32(_) => prop_assert_eq!(got, Some(DataType::Int32)),
            Value::Int64(_) => prop_assert_eq!(got, Some(DataType::Int64)),
            Value::UInt8(_) => prop_assert_eq!(got, Some(DataType::UInt8)),
            Value::UInt16(_) => prop_assert_eq!(got, Some(DataType::UInt16)),
            Value::UInt32(_) => prop_assert_eq!(got, Some(DataType::UInt32)),
            Value::UInt64(_) => prop_assert_eq!(got, Some(DataType::UInt64)),
            Value::Float32(_) => prop_assert_eq!(got, Some(DataType::Float32)),
            Value::Float64(_) => prop_assert_eq!(got, Some(DataType::Float64)),
            Value::BigInt(_) => prop_assert_eq!(got, Some(DataType::BigInt { width: None })),
            Value::Decimal(_) => prop_assert_eq!(
                got,
                Some(DataType::Decimal {
                    precision: None,
                    scale: None,
                })
            ),
            Value::Text(_) => prop_assert_eq!(got, Some(DataType::Text { size: None })),
            Value::Bytes(_) => prop_assert_eq!(got, Some(DataType::Bytes { size: None })),
            Value::Date(_) => prop_assert_eq!(got, Some(DataType::Date)),
            Value::Timestamp(_) => prop_assert_eq!(got, Some(DataType::Timestamp)),
            Value::Uuid(_) => prop_assert_eq!(got, Some(DataType::Uuid)),
            Value::Ipv4(_) => prop_assert_eq!(got, Some(DataType::Ipv4)),
            Value::Ipv6(_) => prop_assert_eq!(got, Some(DataType::Ipv6)),
            Value::Json(_) => prop_assert_eq!(got, Some(DataType::Json)),
            Value::Object(_) => unreachable!("strategy excludes Object"),
            Value::Custom(_) => unreachable!("strategy excludes Custom"),
        }
    }
}
