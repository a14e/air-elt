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
///
/// `Value::Custom` is admitted only when the underlying [`DynType`]
/// declares [`cursor_compatible() == true`](crate::types::dynamic::DynType::cursor_compatible).
/// In that case the wire shape is
/// `{ "type": "custom", "kind": "<kind>", "value": <json> }`, with
/// `<json>` produced by [`DynValue::to_json`]. The bare
/// `Value::Deserialize` impl does NOT decode `custom` envelopes —
/// recovering a `Box<dyn DynValue>` needs the expected descriptor
/// up front. The typed entry point is
/// [`crate::types::DataType::decode_cursor_json`], driven by the
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
    Json(serde_json::Value),
    /// Connector-specific opaque value. Cursor JSON storage admits
    /// this variant iff the underlying `DynType::cursor_compatible()`
    /// returns `true` AND the matching descriptor overrides
    /// `DynType::decode_cursor_value` (the typed reload entry, called
    /// from [`crate::types::DataType::decode_cursor_json`]).
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

    /// Map this `Value` variant onto its concrete [`crate::types::DataType`].
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
    pub fn data_type(&self) -> Option<crate::types::DataType> {
        use crate::types::DataType;
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
            Value::Json(_) => Some(DataType::Json),
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
            Value::Json(j) => emit(serializer, "json", j),
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
            "json" => Ok(Value::Json(v)),
            "custom" => {
                // Custom values can't deserialize through the bare
                // `Value` path: a `Box<dyn DynValue>` needs the
                // descriptor's `decode_cursor_value` impl, which we
                // can only reach when the caller supplies the
                // expected `DataType::Custom(t)` ahead of time. Use
                // [`crate::types::DataType::decode_cursor_json`]
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

    impl crate::types::dynamic::DynType for StubType {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn kind(&self) -> &str {
            "test.stub"
        }
        fn can_convert_to(&self, _t: &crate::types::DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &crate::types::DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &crate::types::DataType,
            _ctx: &crate::types::convert::context::ConversionContext,
        ) -> Result<Value, crate::types::convert::ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &crate::types::DataType,
            _ctx: &crate::types::convert::context::ConversionContext,
        ) -> Result<Value, crate::types::convert::ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn crate::types::dynamic::DynType> {
            Box::new(*self)
        }
    }

    /// Test stand-in `DynValue` so we can construct a `Value::Custom` and
    /// verify the serialize-error guard.
    #[derive(Debug)]
    struct StubValue;

    impl DynValue for StubValue {
        fn dyn_type(&self) -> Box<dyn crate::types::dynamic::DynType> {
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

    impl crate::types::dynamic::DynType for CursorStubType {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn kind(&self) -> &str {
            "test.cursor_stub"
        }
        fn cursor_compatible(&self) -> bool {
            true
        }
        fn can_convert_to(&self, _t: &crate::types::DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &crate::types::DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &crate::types::DataType,
            _ctx: &crate::types::convert::context::ConversionContext,
        ) -> Result<Value, crate::types::convert::ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &crate::types::DataType,
            _ctx: &crate::types::convert::context::ConversionContext,
        ) -> Result<Value, crate::types::convert::ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn crate::types::dynamic::DynType> {
            Box::new(*self)
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CursorStubValue(u32);

    impl DynValue for CursorStubValue {
        fn dyn_type(&self) -> Box<dyn crate::types::dynamic::DynType> {
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
}
