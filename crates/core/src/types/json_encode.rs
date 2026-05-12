//! Debezium-compatible JSON encoder for canonical [`Value`] variants.
//!
//! Used by the JSON auto-pack path (`*:body` mapping) to convert a
//! [`Value`] into a [`serde_json::Value`]. Encoding rules:
//!
//! - Integers: native JSON numbers; `UInt64 > 2^53` becomes a string
//!   (preserves precision through downstream JSON parsers).
//! - Floats: NaN / +Inf / -Inf collapse to `null`.
//! - `BigInt` and `Decimal`: canonical decimal-string form.
//! - `Bytes`: bare lowercase hex (no `0x` prefix, no base64 envelope).
//! - `Date`: ISO `YYYY-MM-DD`.
//! - `Timestamp`: RFC3339 UTC, three fractional digits always
//!   present (`SecondsFormat::Millis`). Aligned with the BSON-bridge
//!   encoder in `commons-mongodb::bson_value::bson_to_json` so the
//!   same logical instant produces identical JSON regardless of which
//!   path emits it.
//! - `Uuid`: canonical hyphenated string.
//! - `Json`: passed through, structurally validated for depth.
//! - `Xml`: not present as a `Value` variant — the canonical pivot
//!   carries XML as text. (Documented for completeness.)
//! - `Custom`: delegates to `DynValue::to_json`.
//!
//! Recursion through nested JSON is depth-tracked, capped at 100
//! (matches `MAX_INFER_DEPTH` in commons-mongodb so anything we can
//! infer we can also encode).
//!
//! ## Forbidden idiom
//!
//! Do not encode a [`Value`] by routing it through its own `Serialize`
//! impl — that wraps every variant in the `{type, value}` cursor
//! envelope. The unit grep test below asserts the literal substring
//! does not appear in this file's source.
//!
//! [`Value`]: crate::types::value::Value

use crate::error::JsonEncodeError;
use crate::types::value::Value;

/// Maximum nesting depth across recursive `Value::Json` payloads.
pub const MAX_JSON_DEPTH: usize = 100;

/// JSON-safe integer ceiling for unsigned 64-bit values. `2^53` is the
/// widely-quoted "safe integer" boundary across JS/TS consumers; values
/// at or above this are emitted as decimal strings.
pub const U64_JSON_SAFE_MAX: u64 = 1_u64 << 53;

/// Encode a canonical [`Value`] as a [`serde_json::Value`] using the
/// rules documented at the module level.
pub fn value_to_json(v: &Value) -> Result<serde_json::Value, JsonEncodeError> {
    encode_value_at_depth(v, 0)
}

fn encode_value_at_depth(v: &Value, depth: usize) -> Result<serde_json::Value, JsonEncodeError> {
    if depth > MAX_JSON_DEPTH {
        return Err(JsonEncodeError::DepthExceeded);
    }
    let out = match v {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int8(n) => serde_json::Value::from(*n),
        Value::Int16(n) => serde_json::Value::from(*n),
        Value::Int32(n) => serde_json::Value::from(*n),
        Value::Int64(n) => serde_json::Value::from(*n),
        Value::UInt8(n) => serde_json::Value::from(*n),
        Value::UInt16(n) => serde_json::Value::from(*n),
        Value::UInt32(n) => serde_json::Value::from(*n),
        Value::UInt64(n) => {
            if *n >= U64_JSON_SAFE_MAX {
                serde_json::Value::String(n.to_string())
            } else {
                serde_json::Value::from(*n)
            }
        }
        Value::Float32(n) => float32_to_json(*n),
        Value::Float64(n) => float64_to_json(*n),
        Value::BigInt(b) => serde_json::Value::String(b.to_str_radix(10)),
        Value::Decimal(d) => serde_json::Value::String(d.to_string()),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::Bytes(bytes) => serde_json::Value::String(bytes_to_hex(bytes)),
        Value::Date(d) => serde_json::Value::String(d.format("%Y-%m-%d").to_string()),
        Value::Timestamp(ts) => {
            serde_json::Value::String(ts.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        }
        Value::Uuid(u) => serde_json::Value::String(u.to_string()),
        Value::Json(inner) => encode_serde_json_at_depth(inner, depth + 1)?,
        Value::Custom(c) => c.to_json()?,
    };
    Ok(out)
}

/// Recursive depth-tracked walk over a nested `serde_json::Value`. We
/// can't trust an external `serde_json::Value` to fit the cap.
fn encode_serde_json_at_depth(
    v: &serde_json::Value,
    depth: usize,
) -> Result<serde_json::Value, JsonEncodeError> {
    if depth > MAX_JSON_DEPTH {
        return Err(JsonEncodeError::DepthExceeded);
    }
    let out = match v {
        serde_json::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(encode_serde_json_at_depth(item, depth + 1)?);
            }
            serde_json::Value::Array(out)
        }
        serde_json::Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                out.insert(k.clone(), encode_serde_json_at_depth(val, depth + 1)?);
            }
            serde_json::Value::Object(out)
        }
        // Scalars don't recurse — clone is cheap (bool/null) or
        // moderate (string/number) but we can't move out of `&`.
        other => other.clone(),
    };
    Ok(out)
}

fn float32_to_json(n: f32) -> serde_json::Value {
    if !n.is_finite() {
        return serde_json::Value::Null;
    }
    serde_json::Number::from_f64(f64::from(n))
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

fn float64_to_json(n: f64) -> serde_json::Value {
    if !n.is_finite() {
        return serde_json::Value::Null;
    }
    serde_json::Number::from_f64(n)
        .map(serde_json::Value::Number)
        .unwrap_or(serde_json::Value::Null)
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::types::dynamic::{DynType, DynValue};
    use bigdecimal::BigDecimal;
    use chrono::{DateTime, NaiveDate, Utc};
    use num_bigint::BigInt;
    use serde_json::json;
    use std::any::Any;
    use std::str::FromStr;
    use uuid::Uuid;

    /// Exhaustive parametrised table.
    #[test]
    fn exact_bytes_table_per_variant() {
        let ts: DateTime<Utc> = "2024-01-15T12:30:45Z".parse().unwrap();
        let date = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        let uuid = Uuid::nil();

        let cases: Vec<(Value, serde_json::Value)> = vec![
            (Value::Null, serde_json::Value::Null),
            (Value::Bool(true), json!(true)),
            (Value::Bool(false), json!(false)),
            (Value::Int16(-7), json!(-7)),
            (Value::Int32(-7), json!(-7)),
            (Value::Int64(-7), json!(-7)),
            (Value::UInt8(7), json!(7)),
            (Value::UInt16(7), json!(7)),
            (Value::UInt32(7), json!(7)),
            (Value::UInt64(7), json!(7)),
            (Value::UInt64(U64_JSON_SAFE_MAX), json!("9007199254740992")),
            (Value::UInt64(u64::MAX), json!("18446744073709551615")),
            (Value::Float32(1.5), json!(1.5)),
            (Value::Float32(f32::NAN), serde_json::Value::Null),
            (Value::Float32(f32::INFINITY), serde_json::Value::Null),
            (Value::Float32(f32::NEG_INFINITY), serde_json::Value::Null),
            (Value::Float64(2.25), json!(2.25)),
            (Value::Float64(f64::NAN), serde_json::Value::Null),
            (Value::Float64(f64::INFINITY), serde_json::Value::Null),
            (
                Value::BigInt(BigInt::from(123_456_789_i64)),
                json!("123456789"),
            ),
            (
                Value::Decimal(BigDecimal::from_str("1.23").unwrap()),
                json!("1.23"),
            ),
            (Value::Text("hi".into()), json!("hi")),
            (
                Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef]),
                json!("deadbeef"),
            ),
            (Value::Bytes(vec![]), json!("")),
            (Value::Date(date), json!("2024-01-15")),
            (Value::Timestamp(ts), json!("2024-01-15T12:30:45.000Z")),
            (
                Value::Uuid(uuid),
                json!("00000000-0000-0000-0000-000000000000"),
            ),
            (
                Value::Json(json!({"k": [1, 2, 3]})),
                json!({"k": [1, 2, 3]}),
            ),
        ];
        for (v, expected) in cases {
            let got = value_to_json(&v).unwrap();
            assert_eq!(got, expected, "encode mismatch for {v:?}");
        }
    }

    /// Custom-type override path (test #36 partial — covered fully in
    /// the per-impl test files).
    #[derive(Debug, Clone)]
    struct StubType;
    impl DynType for StubType {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "stub.t"
        }
        fn can_convert_to(&self, _: &crate::types::DataType, _: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _: &crate::types::DataType, _: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _: Value,
            _: &crate::types::DataType,
            _: &crate::types::convert::context::ConversionContext,
        ) -> Result<Value, crate::types::convert::ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _: Value,
            _: &crate::types::DataType,
            _: &crate::types::convert::context::ConversionContext,
        ) -> Result<Value, crate::types::convert::ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
    }

    #[derive(Debug, Clone)]
    struct StubVal(serde_json::Value);
    impl DynValue for StubVal {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
        fn eq_dyn(&self, _: &dyn DynValue) -> bool {
            true
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(self.clone())
        }
        fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
            Ok(self.0.clone())
        }
    }

    /// `DynValue::to_json` is invoked through the `Custom` arm.
    #[test]
    fn custom_delegates_to_dynvalue_to_json() {
        let v = Value::Custom(Box::new(StubVal(json!({"a": 1}))));
        assert_eq!(value_to_json(&v).unwrap(), json!({"a": 1}));
    }

    /// `DynValue::to_json` default returns an error — the `Custom` arm
    /// must propagate it.
    #[derive(Debug, Clone)]
    struct UnimplVal;
    impl DynValue for UnimplVal {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
        fn eq_dyn(&self, _: &dyn DynValue) -> bool {
            true
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(UnimplVal)
        }
    }

    #[test]
    fn custom_default_to_json_propagates() {
        let v = Value::Custom(Box::new(UnimplVal));
        let res = value_to_json(&v);
        assert!(matches!(res, Err(JsonEncodeError::Variant(_))));
    }

    /// Depth cap. A 101-deep nested array errors; 100 passes.
    #[test]
    fn depth_cap_100_passes_101_fails() {
        fn nest(n: usize) -> serde_json::Value {
            let mut v = serde_json::Value::Array(vec![]);
            for _ in 0..n {
                v = serde_json::Value::Array(vec![v]);
            }
            v
        }
        // 99 nested arrays inside Value::Json → outer arm uses 1
        // depth, then 99 inner walks → max depth reached = 100. Pass.
        let ok = Value::Json(nest(99));
        assert!(value_to_json(&ok).is_ok());

        // 101 nested arrays → exceeds cap. Fail.
        let bad = Value::Json(nest(101));
        assert!(matches!(
            value_to_json(&bad),
            Err(JsonEncodeError::DepthExceeded)
        ));
    }

    /// Anti-shortcut grep test — this file's source must not
    /// contain the forbidden serde-json to-value call that would route
    /// a `Value` through its cursor-envelope `Serialize`.
    #[test]
    fn no_serde_json_to_value_call_in_source() {
        let src = include_str!("json_encode.rs");
        // Construct the forbidden literal from parts so the test file
        // itself doesn't trip the grep.
        let needle = format!("serde_json::{}(", "to_value");
        assert!(
            !src.contains(&needle),
            "json_encode.rs must not call {needle} — \
             it would route Value through its cursor-envelope Serialize"
        );
    }
}
