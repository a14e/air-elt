//! `Json → Text(size)`: serialize JSON to its compact string form, then
//! truncate to the sink's declared size in **characters**. `Json → Text*`
//! (unbounded sink) is the same minus the truncate.

use super::error::ConvertError;
use super::text_truncate::truncate_chars;
use crate::{DataType, Value};

pub fn convert(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let v = match value {
        Value::Json(v) => v,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    // serde_json::Value cannot contain non-finite f64 (Number::from_f64
    // rejects NaN/±Inf at construction time), so to_string is infallible.
    let serialized =
        serde_json::to_string(&v).expect("serde_json::Value always serializes successfully");
    let out = match sink_size {
        None => serialized,
        Some(max) => truncate_chars(&serialized, max as usize).to_string(),
    };
    Ok(Value::Text(out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn value_shape_mismatch() {
        let res = convert(Value::Text("abc".into()), &DataType::Json, None);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn serializes_array() {
        let v = serde_json::json!([1, 2, 3]);
        let out = convert(Value::Json(v), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("[1,2,3]".into()));
    }

    #[test]
    fn serializes_nested_object() {
        let v = serde_json::json!({"a": {"b": [1]}});
        let out = convert(Value::Json(v), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("{\"a\":{\"b\":[1]}}".into()));
    }

    #[test]
    fn serializes_null_payload() {
        let out = convert(Value::Json(serde_json::Value::Null), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("null".into()));
    }

    #[test]
    fn truncates_bounded_sink_in_chars() {
        // Object serializes to `{"a":1,"b":2}` (13 chars). Truncate to 5.
        let v = serde_json::json!({"a": 1, "b": 2});
        let out = convert(Value::Json(v), &DataType::Json, Some(5)).unwrap();
        assert_eq!(out, Value::Text("{\"a\":".into()));
    }

    #[test]
    fn unbounded_passthrough() {
        let v = serde_json::json!({"x": "hello"});
        let out = convert(Value::Json(v), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("{\"x\":\"hello\"}".into()));
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Recursive `serde_json::Value` strategy spanning all canonical
    /// JSON types. `serde_json::Number::from_f64` rejects NaN / ±Inf at
    /// construction; the float arm is additionally restricted to the
    /// safe-double-precision integer range (`-2^53..2^53`) so the
    /// ryu-emitted decimal string parses back to the same `f64` bits
    /// regardless of any future serde_json `Number` representation
    /// quirks. This keeps the round-trip property focused on canonical
    /// JSON shape coverage rather than float-formatting edge cases.
    fn arb_json_value() -> impl Strategy<Value = serde_json::Value> {
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            any::<i64>().prop_map(|n| serde_json::Value::Number(n.into())),
            any::<u64>().prop_map(|n| serde_json::Value::Number(n.into())),
            // Floats that round-trip cleanly through JSON. We use small
            // tenths (`n / 10.0` for `n` in `-10_000..10_000`) so each
            // generated number has a short, unambiguous decimal form:
            // ryu emits e.g. "12.3", which parses back to the same
            // bits. This keeps the property focused on JSON shape
            // coverage rather than f64 textual-formatting edge cases
            // (very large exponents can drift across `to_string` /
            // `from_str` despite ryu's shortest-roundtrip guarantee
            // when combined with serde_json's `Number::eq` semantics).
            (-10_000i32..10_000i32).prop_map(|n| {
                let f = f64::from(n) / 10.0;
                serde_json::Number::from_f64(f)
                    .map(serde_json::Value::Number)
                    .unwrap_or(serde_json::Value::Null)
            }),
            "\\PC{0,8}".prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(
            3,  // max depth
            16, // size budget
            8,  // max items per collection
            |inner| {
                prop_oneof![
                    prop::collection::vec(inner.clone(), 0..6).prop_map(serde_json::Value::Array),
                    prop::collection::vec(("\\PC{0,8}", inner), 0..6)
                        .prop_map(|kvs| serde_json::Value::Object(kvs.into_iter().collect())),
                ]
            },
        )
    }

    /// `Value::Json(v) → Value::Text(s)` followed by `serde_json::from_str(&s)`
    /// recovers the original JSON value, for the *small-tenths* float
    /// regime. Floats here are constrained to `n / 10.0` for
    /// `n ∈ -10_000..10_000` so each generated number has a short,
    /// unambiguous decimal form that ryu emits identically and
    /// `from_str` parses back to the same bits. Broader f64 coverage —
    /// including high-magnitude finite values — is exercised by
    /// `json_text_round_trip_high_magnitude_finite_floats`.
    #[test_strategy::proptest]
    fn json_text_round_trip_for_small_decimal_floats(
        #[strategy(arb_json_value())] v: serde_json::Value,
    ) {
        let out = convert(Value::Json(v.clone()), &DataType::Json, None).unwrap();
        let Value::Text(s) = out else {
            prop_assert!(false, "expected Value::Text");
            return Ok(());
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("converter output must be valid JSON");
        prop_assert_eq!(parsed, v);
    }

    /// Round-trip property over the FULL finite f64 range with strict
    /// bit equality. Relies on `serde_json`'s `float_roundtrip` feature
    /// (enabled in the root workspace manifest), which routes parsing
    /// through `lexical-core` so every finite f64 ↔ shortest decimal
    /// is bijective. See <https://github.com/serde-rs/json/issues/536>.
    #[test_strategy::proptest(ProptestConfig::with_cases(512))]
    fn json_text_round_trip_finite_floats(
        #[strategy(any::<f64>().prop_filter("finite", |x| x.is_finite()))] f: f64,
    ) {
        let Some(num) = serde_json::Number::from_f64(f) else {
            return Ok(());
        };
        let v = serde_json::Value::Number(num);
        let out = convert(Value::Json(v.clone()), &DataType::Json, None).unwrap();
        let Value::Text(s) = out else {
            prop_assert!(false, "expected Value::Text");
            return Ok(());
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&s).expect("converter output must be valid JSON");
        prop_assert_eq!(parsed, v);
    }
}
