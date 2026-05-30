//! Canonical text rendering of a [`Value`] — the single source of truth shared
//! by string interpolation (`"x = {expr}"` / `toStringCast`) and the
//! `* → Text` conversion dispatch.
//!
//! [`value_to_string`] renders every variant to its canonical text form; it is
//! total and infallible. [`convert`] is the `* → Text` executor: it renders,
//! then for a bounded sink applies a **value-aware** truncation — a rendering
//! that already fits needs no `truncate` consent (mirrors the value-aware
//! `Decimal → Decimal` path), only an over-long one is a lossy cut requiring it.
//!
//! Rendering notes worth pinning:
//! * `Null` → empty string (interpolation contributes nothing for a missing
//!   value; the conversion path never reaches here with `Null` — it is handled
//!   by default-substitution earlier in [`dispatch`](super::dispatch)).
//! * `Bytes` → lowercase hex (shared with the JSON encoder via
//!   [`bytes_to_hex`](super::utils::bytes_to_hex), so binary renders
//!   identically everywhere).
//! * `Timestamp` → RFC 3339; `Custom` → its canonical JSON encoding (bare
//!   string when the encoding is a top-level JSON string, else JSON, else the
//!   `Debug` fallback).

use super::utils::{bytes_to_hex, truncate_to_chars};
use crate::Value;
use crate::dynamic::DynValue;
use crate::json_encode::value_to_json;

/// Render `value` to its canonical text form (the form an interpolation
/// segment produces and the `* → Text` conversion emits).
pub fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(inner) => inner.to_string(),
        Value::Int8(inner) => inner.to_string(),
        Value::Int16(inner) => inner.to_string(),
        Value::Int32(inner) => inner.to_string(),
        Value::Int64(inner) => inner.to_string(),
        Value::UInt8(inner) => inner.to_string(),
        Value::UInt16(inner) => inner.to_string(),
        Value::UInt32(inner) => inner.to_string(),
        Value::UInt64(inner) => inner.to_string(),
        Value::Float32(inner) => inner.to_string(),
        Value::Float64(inner) => inner.to_string(),
        Value::Text(inner) => inner.clone(),
        Value::BigInt(inner) => inner.to_string(),
        Value::Decimal(inner) => inner.to_string(),
        Value::Uuid(inner) => inner.to_string(),
        Value::Date(inner) => inner.to_string(),
        Value::Timestamp(inner) => inner.to_rfc3339(),
        Value::Bytes(inner) => bytes_to_hex(inner),
        Value::Ipv4(inner) => inner.to_string(),
        Value::Ipv6(inner) => inner.to_string(),
        Value::Json(inner) => inner.to_string(),
        Value::Object(entries) => {
            // `value_to_string` is total/infallible by contract, so a field
            // that fails to JSON-encode (a degenerate `Custom` whose `to_json`
            // errors, or depth overflow) renders as `null` rather than aborting
            // — the same best-effort fallback interpolation has always used.
            // The fallible `Object → Json` dispatch arm propagates that error
            // instead; the two diverge only on this pathological field.
            let map: serde_json::Map<String, serde_json::Value> = entries
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value_to_json(value).unwrap_or(serde_json::Value::Null),
                    )
                })
                .collect();
            serde_json::Value::Object(map).to_string()
        }
        Value::Custom(inner) => custom_to_string(inner.as_ref()),
    }
}

/// Canonical string form of an opaque [`Value::Custom`]. Custom types expose
/// their canonical encoding through [`DynValue::to_json`] (the same encoder the
/// JSON-pack path uses); a top-level JSON string renders bare (e.g. an ObjectId
/// hex), other shapes render as JSON, matching the [`Value::Json`] arm. A custom
/// type that does not implement `to_json` falls back to its `Debug` form.
fn custom_to_string(value: &dyn DynValue) -> String {
    match value.to_json() {
        Ok(serde_json::Value::String(text)) => text,
        Ok(json) => json.to_string(),
        Err(_) => format!("{value:?}"),
    }
}

/// `* → Text` conversion: render the value, then truncate to the sink's
/// character bound (a no-op when the rendering already fits). Infallible — the
/// **matrix** is the gatekeeper: it admits `* → Text(None)` losslessly and
/// `* → Text(n)` only under `truncate` (a possibly-lossy cut), so by the time a
/// bounded conversion reaches here the consent was already given at validation.
pub(crate) fn convert(value: Value, size: Option<u32>) -> Value {
    let rendered = value_to_string(&value);
    match size {
        None => Value::Text(rendered),
        Some(max) => Value::Text(truncate_to_chars(rendered, max as usize)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::any::Any;

    use serde_json::json;

    use super::*;
    use crate::DataType;
    use crate::convert::context::ConversionContext;
    use crate::convert::error::ConvertError;
    use crate::dynamic::DynType;
    use crate::error::JsonEncodeError;

    // ---- value_to_string: canonical rendering -----------------------------

    #[test]
    fn null_renders_to_empty_string() {
        assert_eq!(value_to_string(&Value::Null), "");
    }

    #[test]
    fn scalars_render_to_their_natural_form() {
        assert_eq!(value_to_string(&Value::Int64(42)), "42");
        assert_eq!(value_to_string(&Value::Bool(true)), "true");
        assert_eq!(value_to_string(&Value::Text("hi".into())), "hi");
        assert_eq!(value_to_string(&Value::Float64(1.5)), "1.5");
    }

    #[test]
    fn timestamp_renders_as_rfc3339() {
        use chrono::{TimeZone, Utc};
        let timestamp = Utc.timestamp_opt(0, 0).unwrap();
        assert_eq!(
            value_to_string(&Value::Timestamp(timestamp)),
            "1970-01-01T00:00:00+00:00"
        );
    }

    #[test]
    fn bytes_render_as_lowercase_hex() {
        // Shared with the JSON encoder — binary renders identically everywhere.
        assert_eq!(
            value_to_string(&Value::Bytes(vec![0x01, 0xab, 0xff])),
            "01abff"
        );
        assert_eq!(value_to_string(&Value::Bytes(vec![])), "");
    }

    #[test]
    fn renders_decimal_bigint_date_ip_canonically() {
        // `value_to_string` is the public renderer behind `toStringCast` and
        // interpolation, so pin the canonical form of every newly text-bound
        // variant (not just Int/Bool/Float) against silent formatting drift.
        use std::net::{Ipv4Addr, Ipv6Addr};
        use std::str::FromStr;

        assert_eq!(
            value_to_string(&Value::Decimal(
                bigdecimal::BigDecimal::from_str("1.50").unwrap()
            )),
            "1.50",
            "decimal scale preserved"
        );
        assert_eq!(
            value_to_string(&Value::BigInt(num_bigint::BigInt::from(-42))),
            "-42"
        );
        assert_eq!(
            value_to_string(&Value::Date(
                chrono::NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()
            )),
            "2024-01-15"
        );
        assert_eq!(
            value_to_string(&Value::Ipv4(Ipv4Addr::new(192, 0, 2, 1))),
            "192.0.2.1"
        );
        assert_eq!(
            value_to_string(&Value::Ipv6("2001:db8::1".parse::<Ipv6Addr>().unwrap())),
            "2001:db8::1"
        );
        assert_eq!(value_to_string(&Value::Float32(1.5)), "1.5");
    }

    // ---- Custom: canonical rendering via DynValue::to_json ----------------

    #[derive(Debug, Clone)]
    struct StubType;
    impl DynType for StubType {
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn kind(&self) -> &str {
            "stub.t"
        }
        fn can_convert_to(&self, _: &DataType, _: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _: &DataType, _: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _: Value,
            _: &DataType,
            _: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _: Value,
            _: &DataType,
            _: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
    }

    #[derive(Debug, Clone)]
    struct StubVal(Result<serde_json::Value, ()>);
    impl DynValue for StubVal {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(StubType)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn Any> {
            self
        }
        fn is_equal(&self, _: &dyn DynValue) -> bool {
            true
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(self.clone())
        }
        fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
            self.0
                .clone()
                .map_err(|()| JsonEncodeError::Variant("unimplemented".to_string()))
        }
    }

    #[test]
    fn custom_string_json_renders_bare() {
        // A top-level JSON string (e.g. an ObjectId hex) renders without quotes.
        let value = Value::Custom(Box::new(StubVal(Ok(json!("507f1f77bcf86cd799439011")))));
        assert_eq!(value_to_string(&value), "507f1f77bcf86cd799439011");
    }

    #[test]
    fn custom_structured_json_renders_as_json() {
        let value = Value::Custom(Box::new(StubVal(Ok(json!({"a": 1})))));
        assert_eq!(value_to_string(&value), "{\"a\":1}");
    }

    #[test]
    fn custom_without_to_json_falls_back_to_debug() {
        let value = Value::Custom(Box::new(StubVal(Err(()))));
        assert_eq!(value_to_string(&value), "StubVal(Err(()))");
    }

    // ---- convert: * → Text dispatch ---------------------------------------
    //
    // `convert` is infallible: it renders, then truncates to the sink bound (a
    // no-op when the rendering already fits). The matrix is the gatekeeper for
    // whether a bounded `* → Text` is allowed at all (lossless when it fits,
    // truncate-gated otherwise), so `convert` itself never re-checks consent.

    #[test]
    fn convert_unbounded_is_lossless_render() {
        assert_eq!(
            convert(Value::Int64(12345), None),
            Value::Text("12345".into())
        );
    }

    #[test]
    fn convert_bounded_fits_is_unchanged() {
        // "12345" is 5 chars; sink Text(10) fits → returned whole.
        assert_eq!(
            convert(Value::Int64(12345), Some(10)),
            Value::Text("12345".into())
        );
    }

    #[test]
    fn convert_bounded_overflow_truncates_to_chars() {
        assert_eq!(
            convert(Value::Int64(12345), Some(3)),
            Value::Text("123".into())
        );
    }

    #[test]
    fn convert_bytes_to_text_is_hex() {
        assert_eq!(
            convert(Value::Bytes(vec![0xde, 0xad]), None),
            Value::Text("dead".into())
        );
    }

    #[test]
    fn convert_uuid_to_text_is_hyphenated_and_lossless() {
        use ::uuid::Uuid;
        let u = Uuid::parse_str("507f1f77-bcf8-6cd7-9943-9011aabbccdd").unwrap();
        assert_eq!(
            convert(Value::Uuid(u), Some(36)),
            Value::Text("507f1f77-bcf8-6cd7-9943-9011aabbccdd".into())
        );
    }

    #[test]
    fn convert_json_to_text_serializes_and_truncates() {
        let v = json!({"a": 1, "b": 2});
        assert_eq!(
            convert(Value::Json(v), Some(5)),
            Value::Text("{\"a\":".into())
        );
    }

    // ---- JSON → Text serialization round-trips (ported from json_text) ----

    use proptest::prelude::*;

    /// Recursive `serde_json::Value` strategy spanning all canonical JSON
    /// types. `serde_json::Number::from_f64` rejects NaN / ±Inf at
    /// construction; the small-tenths float arm keeps the textual form
    /// unambiguous so the round-trip focuses on JSON shape coverage.
    fn arb_json_value() -> impl Strategy<Value = serde_json::Value> {
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            any::<i64>().prop_map(|n| serde_json::Value::Number(n.into())),
            any::<u64>().prop_map(|n| serde_json::Value::Number(n.into())),
            (-10_000i32..10_000i32).prop_map(|n| {
                let f = f64::from(n) / 10.0;
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }),
            "\\PC{0,8}".prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(3, 16, 8, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(serde_json::Value::Array),
                prop::collection::vec(("\\PC{0,8}", inner), 0..6)
                    .prop_map(|kvs| serde_json::Value::Object(kvs.into_iter().collect())),
            ]
        })
    }

    /// `Value::Json(v)` → `Value::Text(s)` then `from_str(&s)` recovers `v`,
    /// for the small-tenths float regime.
    #[test_strategy::proptest]
    fn json_text_round_trip_for_small_decimal_floats(
        #[strategy(arb_json_value())] v: serde_json::Value,
    ) {
        let out = convert(Value::Json(v.clone()), None);
        let Value::Text(s) = out else {
            prop_assert!(false, "expected Value::Text");
            return Ok(());
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("converter output must be valid JSON");
        prop_assert_eq!(parsed, v);
    }

    /// Round-trip over the FULL finite f64 range with strict bit equality
    /// (relies on serde_json's `float_roundtrip` feature).
    #[test_strategy::proptest(ProptestConfig::with_cases(512))]
    fn json_text_round_trip_finite_floats(
        #[strategy(any::<f64>().prop_filter("finite", |x| x.is_finite()))] f: f64,
    ) {
        let Some(num) = serde_json::Number::from_f64(f) else {
            return Ok(());
        };
        let v = serde_json::Value::Number(num);
        let out = convert(Value::Json(v.clone()), None);
        let Value::Text(s) = out else {
            prop_assert!(false, "expected Value::Text");
            return Ok(());
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("converter output must be valid JSON");
        prop_assert_eq!(parsed, v);
    }
}
