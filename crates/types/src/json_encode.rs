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
//! [`Value`]: crate::value::Value

use crate::convert::utils::bytes_to_hex;
use crate::error::JsonEncodeError;
use crate::value::Value;

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
        Value::Ipv4(a) => serde_json::Value::String(a.to_string()),
        Value::Ipv6(a) => serde_json::Value::String(a.to_string()),
        Value::Json(inner) => encode_serde_json_at_depth(inner, depth + 1)?,
        Value::Object(entries) => {
            let mut map = serde_json::Map::with_capacity(entries.len());
            for (key, val) in entries {
                map.insert(key.clone(), encode_value_at_depth(val, depth + 1)?);
            }
            serde_json::Value::Object(map)
        }
        Value::Custom(c) => c.to_json()?,
        // Intervals have no canonical JSON form (deliberately minimal — no
        // conversions). They only ever type the Redis sink `ttl` column,
        // which the sink reads directly; an interval nested inside a
        // JSON-bound value is an authoring error, surfaced here.
        Value::Interval(_) => {
            return Err(JsonEncodeError::Variant(
                "interval has no JSON encoding rule".to_string(),
            ));
        }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::dynamic::{DynType, DynValue};
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
                Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 1)),
                json!("192.0.2.1"),
            ),
            (
                Value::Ipv6("2001:db8::1".parse().unwrap()),
                json!("2001:db8::1"),
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
        fn can_convert_to(&self, _: &crate::DataType, _: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _: &crate::DataType, _: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _: Value,
            _: &crate::DataType,
            _: &crate::convert::context::ConversionContext,
        ) -> Result<Value, crate::convert::ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _: Value,
            _: &crate::DataType,
            _: &crate::convert::context::ConversionContext,
        ) -> Result<Value, crate::convert::ConvertError> {
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
        fn is_equal(&self, _: &dyn DynValue) -> bool {
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
        fn is_equal(&self, _: &dyn DynValue) -> bool {
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

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Yields any non-finite `f32` — covers the full NaN / Inf
    /// bit-space, not just the three canonical patterns. IEEE-754
    /// binary32: bit 31 = sign, bits 23..=30 = exponent, bits 0..=22 =
    /// mantissa. Setting the exponent to all-ones produces ±INFINITY
    /// (mantissa = 0) and every signalling/quiet NaN (mantissa != 0).
    /// Constructing the bits directly avoids `prop_filter` blowing the
    /// 65 536 local-reject budget for a ~1-in-2048 occurrence.
    fn non_finite_f32() -> impl Strategy<Value = f32> {
        (any::<bool>(), any::<u32>()).prop_map(|(sign, mantissa)| {
            let sign_bit = u32::from(sign) << 31;
            let exponent_bits = 0xFF_u32 << 23;
            let mantissa_bits = mantissa & 0x007F_FFFF;
            f32::from_bits(sign_bit | exponent_bits | mantissa_bits)
        })
    }

    /// Yields any non-finite `f64` — IEEE-754 binary64 with the
    /// 11-bit exponent forced to all-ones. Same bit-space coverage as
    /// the `f32` strategy.
    fn non_finite_f64() -> impl Strategy<Value = f64> {
        (any::<bool>(), any::<u64>()).prop_map(|(sign, mantissa)| {
            let sign_bit = u64::from(sign) << 63;
            let exponent_bits = 0x7FF_u64 << 52;
            let mantissa_bits = mantissa & 0x000F_FFFF_FFFF_FFFF;
            f64::from_bits(sign_bit | exponent_bits | mantissa_bits)
        })
    }

    /// Decode a lowercase hex string back into bytes. Test-only inverse
    /// of `bytes_to_hex`; used to lock the round-trip contract.
    fn hex_decode(s: &str) -> Vec<u8> {
        assert!(s.len().is_multiple_of(2), "hex length must be even");
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(s.len() / 2);
        for chunk in bytes.chunks(2) {
            let hi = hex_nibble(chunk[0]);
            let lo = hex_nibble(chunk[1]);
            out.push((hi << 4) | lo);
        }
        out
    }

    fn hex_nibble(b: u8) -> u8 {
        match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            _ => panic!("non-hex byte: {b}"),
        }
    }

    /// Build a `serde_json::Value` nested to `depth` layers, alternating
    /// between object and array containers. Used to exercise the
    /// `Object`/`Array` branches inside `encode_serde_json_at_depth`.
    fn mixed_nest(depth: usize) -> serde_json::Value {
        let mut v = serde_json::Value::Null;
        for level in 0..depth {
            if level.is_multiple_of(2) {
                v = serde_json::Value::Array(vec![v]);
            } else {
                let mut map = serde_json::Map::new();
                map.insert("k".to_string(), v);
                v = serde_json::Value::Object(map);
            }
        }
        v
    }

    /// Walk a `serde_json::Value` and return its maximum container
    /// nesting depth (scalars = 0). Used to assert structural
    /// preservation in the array/object property.
    fn structural_depth(v: &serde_json::Value) -> usize {
        match v {
            serde_json::Value::Array(items) => {
                1 + items.iter().map(structural_depth).max().unwrap_or(0)
            }
            serde_json::Value::Object(map) => {
                1 + map.values().map(structural_depth).max().unwrap_or(0)
            }
            _ => 0,
        }
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(64))]
    fn value_to_json_non_finite_float32_to_null(#[strategy(non_finite_f32())] x: f32) {
        let result = value_to_json(&Value::Float32(x)).expect("encode");
        prop_assert_eq!(result, serde_json::Value::Null);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(64))]
    fn value_to_json_non_finite_float64_to_null(#[strategy(non_finite_f64())] x: f64) {
        let result = value_to_json(&Value::Float64(x)).expect("encode");
        prop_assert_eq!(result, serde_json::Value::Null);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn value_to_json_uint64_safe_window(#[strategy(any::<u64>())] n: u64) {
        let result = value_to_json(&Value::UInt64(n)).expect("encode");
        if n >= U64_JSON_SAFE_MAX {
            prop_assert_eq!(result, serde_json::Value::String(n.to_string()));
        } else {
            prop_assert_eq!(result, serde_json::Value::from(n));
        }
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(100))]
    fn depth_cap_monotone_under(#[strategy(0usize..=99)] n: usize) {
        fn nest(n: usize) -> serde_json::Value {
            let mut v = serde_json::Value::Array(vec![]);
            for _ in 0..n {
                v = serde_json::Value::Array(vec![v]);
            }
            v
        }
        let value = Value::Json(nest(n));
        prop_assert!(value_to_json(&value).is_ok());
    }

    /// Mixed Array/Object nesting under the depth cap must encode
    /// successfully and preserve the structural depth. The pre-existing
    /// `depth_cap_monotone_under` exercises only the `Array` branch;
    /// this one walks both `Array` and `Object` arms inside
    /// `encode_serde_json_at_depth`.
    #[test_strategy::proptest(ProptestConfig::with_cases(64))]
    fn json_encode_array_depth_inside_value_json(#[strategy(0usize..=50)] depth: usize) {
        let input = mixed_nest(depth);
        let expected_depth = structural_depth(&input);
        let encoded = value_to_json(&Value::Json(input)).expect("encode");
        prop_assert_eq!(structural_depth(&encoded), expected_depth);
    }

    /// Hex round-trip property: `hex_decode(bytes_to_hex(v)) == v` and
    /// the encoded string is exactly twice as long as the input. Locks
    /// the canonical lowercase-hex contract for `Value::Bytes`.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn json_encode_bytes_to_hex_round_trip(
        #[strategy(prop::collection::vec(any::<u8>(), 0usize..=64))] bytes: Vec<u8>,
    ) {
        let encoded = value_to_json(&Value::Bytes(bytes.clone())).expect("encode");
        let hex = match encoded {
            serde_json::Value::String(s) => s,
            other => {
                prop_assert!(false, "expected string, got {other:?}");
                unreachable!()
            }
        };
        prop_assert_eq!(hex.len(), bytes.len() * 2);
        prop_assert_eq!(hex_decode(&hex), bytes);
    }

    /// Array-in-object-in-array at the 100/101 boundary: at depth 100
    /// the encoder must accept the payload, at depth 101 it must reject
    /// with `DepthExceeded`. Exercises the `Object` arm in the depth
    /// counter — the plain-array boundary test only covers `Array`.
    #[test]
    fn json_encode_mixed_nesting_depth_at_boundary() {
        // `Value::Json` consumes one depth slot, then 99 mixed layers
        // → max depth reached = 100. Pass.
        let ok = Value::Json(mixed_nest(99));
        assert!(value_to_json(&ok).is_ok());

        // 101 mixed layers → exceeds cap. Fail.
        let bad = Value::Json(mixed_nest(101));
        assert!(matches!(
            value_to_json(&bad),
            Err(JsonEncodeError::DepthExceeded)
        ));
    }
}
