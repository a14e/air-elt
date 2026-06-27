//! Cross-numeric-aware comparison and equality for [`Value`].
//!
//! Unlike `PartialEq for Value` (which is variant-exact), these functions
//! promote integers to a common width before comparing. `Int8(5)` and
//! `Int64(5)` are considered equal; `Int64(5)` and `BigInt(5)` likewise.

use std::cmp::Ordering;

use num_bigint::BigInt;
use num_traits::ToPrimitive;

use crate::Value;

/// Compare two Values with cross-numeric promotion.
/// Returns `None` for incompatible types or NaN comparisons.
pub fn compare_values(a: &Value, b: &Value) -> Option<Ordering> {
    match (a, b) {
        (Value::Null, Value::Null) => Some(Ordering::Equal),

        // Same-type fast paths
        (Value::Int64(a), Value::Int64(b)) => Some(a.cmp(b)),
        (Value::Float64(a), Value::Float64(b)) => a.partial_cmp(b),
        (Value::Text(a), Value::Text(b)) => Some(a.cmp(b)),
        (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
        (Value::BigInt(a), Value::BigInt(b)) => Some(a.cmp(b)),
        (Value::Decimal(a), Value::Decimal(b)) => Some(a.cmp(b)),
        (Value::Date(a), Value::Date(b)) => Some(a.cmp(b)),
        (Value::Timestamp(a), Value::Timestamp(b)) => Some(a.cmp(b)),
        (Value::Bytes(a), Value::Bytes(b)) => Some(a.cmp(b)),
        (Value::Uuid(a), Value::Uuid(b)) => Some(a.cmp(b)),
        (Value::Ipv4(a), Value::Ipv4(b)) => Some(a.cmp(b)),
        (Value::Ipv6(a), Value::Ipv6(b)) => Some(a.cmp(b)),

        // Small ints → i64 promotion
        (Value::Int8(a), _) => compare_values(&Value::Int64(*a as i64), b),
        (_, Value::Int8(b)) => compare_values(a, &Value::Int64(*b as i64)),
        (Value::Int16(a), _) => compare_values(&Value::Int64(*a as i64), b),
        (_, Value::Int16(b)) => compare_values(a, &Value::Int64(*b as i64)),
        (Value::Int32(a), _) => compare_values(&Value::Int64(*a as i64), b),
        (_, Value::Int32(b)) => compare_values(a, &Value::Int64(*b as i64)),
        (Value::UInt8(a), _) => compare_values(&Value::Int64(*a as i64), b),
        (_, Value::UInt8(b)) => compare_values(a, &Value::Int64(*b as i64)),
        (Value::UInt16(a), _) => compare_values(&Value::Int64(*a as i64), b),
        (_, Value::UInt16(b)) => compare_values(a, &Value::Int64(*b as i64)),
        (Value::UInt32(a), _) => compare_values(&Value::Int64(*a as i64), b),
        (_, Value::UInt32(b)) => compare_values(a, &Value::Int64(*b as i64)),

        // Cross-numeric: Int64 ↔ BigInt
        (Value::Int64(a), Value::BigInt(b)) => Some(BigInt::from(*a).cmp(b)),
        (Value::BigInt(a), Value::Int64(b)) => Some(a.cmp(&BigInt::from(*b))),

        // Cross-numeric: Int64 ↔ Float64
        (Value::Int64(a), Value::Float64(b)) => (*a as f64).partial_cmp(b),
        (Value::Float64(a), Value::Int64(b)) => a.partial_cmp(&(*b as f64)),

        // Cross-numeric: BigInt ↔ Float64
        (Value::BigInt(a), Value::Float64(b)) => a.to_f64().and_then(|af| af.partial_cmp(b)),
        (Value::Float64(a), Value::BigInt(b)) => b.to_f64().and_then(|bf| a.partial_cmp(&bf)),

        // Float32 → Float64 widening
        (Value::Float32(a), _) => compare_values(&Value::Float64(*a as f64), b),
        (_, Value::Float32(b)) => compare_values(a, &Value::Float64(*b as f64)),

        // UInt64 → BigInt (may exceed i64)
        (Value::UInt64(a), _) => compare_values(&Value::BigInt(BigInt::from(*a)), b),
        (_, Value::UInt64(b)) => compare_values(a, &Value::BigInt(BigInt::from(*b))),

        // Cross-IP: Ipv4 → Ipv4-mapped Ipv6
        (Value::Ipv4(a), Value::Ipv6(b)) => {
            let mapped = a.to_ipv6_mapped();
            Some(mapped.cmp(b))
        }
        (Value::Ipv6(a), Value::Ipv4(b)) => {
            let mapped = b.to_ipv6_mapped();
            Some(a.cmp(&mapped))
        }

        // Unordered structural types: equality only
        (Value::Json(a), Value::Json(b)) => (a == b).then_some(Ordering::Equal),
        (Value::Object(a), Value::Object(b)) => (a == b).then_some(Ordering::Equal),

        // Interval: total order on the underlying Duration.
        (Value::Interval(a), Value::Interval(b)) => Some(a.cmp(b)),

        // Custom: delegate to DynValue (ordering first, equality fallback)
        (Value::Custom(a), Value::Custom(b)) => {
            if let Some(ord) = a.partial_cmp(&**b) {
                Some(ord)
            } else if a.is_equal(&**b) {
                Some(Ordering::Equal)
            } else {
                None
            }
        }

        _ => None,
    }
}

/// Cross-numeric-aware equality. Unlike `Value::eq` (variant-exact),
/// this treats `Int64(5)` and `BigInt(5)` as equal.
pub fn values_equal(a: &Value, b: &Value) -> bool {
    compare_values(a, b) == Some(Ordering::Equal)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::cmp::Ordering::*;

    #[test]
    fn same_type_int64() {
        assert_eq!(
            compare_values(&Value::Int64(5), &Value::Int64(5)),
            Some(Equal)
        );
        assert_eq!(
            compare_values(&Value::Int64(3), &Value::Int64(7)),
            Some(Less)
        );
    }

    #[test]
    fn cross_int64_bigint() {
        let a = Value::Int64(42);
        let b = Value::BigInt(BigInt::from(42));
        assert!(values_equal(&a, &b));
        assert_eq!(compare_values(&a, &b), Some(Equal));
    }

    #[test]
    fn cross_int64_float64() {
        assert_eq!(
            compare_values(&Value::Int64(3), &Value::Float64(3.0)),
            Some(Equal)
        );
        assert_eq!(
            compare_values(&Value::Int64(3), &Value::Float64(3.5)),
            Some(Less)
        );
    }

    #[test]
    fn nan_returns_none() {
        assert_eq!(
            compare_values(&Value::Float64(f64::NAN), &Value::Float64(1.0)),
            None
        );
    }

    #[test]
    fn date_ordering() {
        use chrono::NaiveDate;
        let a = Value::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap());
        let b = Value::Date(NaiveDate::from_ymd_opt(2024, 6, 15).unwrap());
        assert_eq!(compare_values(&a, &b), Some(Less));
    }

    #[test]
    fn small_int_promotion() {
        assert!(values_equal(&Value::Int8(5), &Value::Int64(5)));
        assert!(values_equal(
            &Value::Int32(100),
            &Value::BigInt(BigInt::from(100))
        ));
        assert!(values_equal(&Value::UInt16(42), &Value::Int64(42)));
    }

    #[test]
    fn incompatible_types_return_none() {
        assert_eq!(
            compare_values(&Value::Text("a".into()), &Value::Int64(1)),
            None
        );
    }

    #[test]
    fn decimal_ordering() {
        use bigdecimal::BigDecimal;
        use std::str::FromStr;
        let a = Value::Decimal(BigDecimal::from_str("1.5").unwrap());
        let b = Value::Decimal(BigDecimal::from_str("2.5").unwrap());
        assert_eq!(compare_values(&a, &b), Some(Less));
    }

    #[test]
    fn text_ordering() {
        assert_eq!(
            compare_values(&Value::Text("abc".into()), &Value::Text("def".into())),
            Some(Less)
        );
    }

    #[test]
    fn null_equals_null() {
        assert_eq!(compare_values(&Value::Null, &Value::Null), Some(Equal));
        assert!(values_equal(&Value::Null, &Value::Null));
    }

    #[test]
    fn null_vs_non_null_returns_none() {
        assert_eq!(compare_values(&Value::Null, &Value::Int64(1)), None);
    }

    #[test]
    fn json_structural_equality() {
        let a = Value::Json(serde_json::json!({"x": 1}));
        let b = Value::Json(serde_json::json!({"x": 1}));
        let c = Value::Json(serde_json::json!({"x": 2}));
        assert!(values_equal(&a, &b));
        assert!(!values_equal(&a, &c));
        assert_eq!(compare_values(&a, &c), None);
    }

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        // ── Value generation strategies ────────────────────────────────

        fn arb_i64() -> impl Strategy<Value = i64> {
            prop_oneof![
                Just(0i64),
                Just(1),
                Just(-1),
                Just(i64::MIN),
                Just(i64::MAX),
                any::<i64>(),
            ]
        }

        /// Strategy for numeric Value variants (all integer widths, floats, bigint, decimal).
        /// Floats include NaN, Inf, -Inf, subnormals, and +/-0.0.
        fn arb_numeric_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                any::<i8>().prop_map(Value::Int8),
                any::<i16>().prop_map(Value::Int16),
                any::<i32>().prop_map(Value::Int32),
                any::<i64>().prop_map(Value::Int64),
                any::<u8>().prop_map(Value::UInt8),
                any::<u16>().prop_map(Value::UInt16),
                any::<u32>().prop_map(Value::UInt32),
                any::<u64>().prop_map(Value::UInt64),
                arb_float32().prop_map(Value::Float32),
                arb_float64().prop_map(Value::Float64),
                arb_bigint().prop_map(Value::BigInt),
                arb_decimal().prop_map(Value::Decimal),
            ]
        }

        fn arb_float32() -> impl Strategy<Value = f32> {
            prop_oneof![
                Just(0.0f32),
                Just(-0.0f32),
                Just(f32::NAN),
                Just(f32::INFINITY),
                Just(f32::NEG_INFINITY),
                Just(f32::MIN_POSITIVE), // smallest normal
                Just(5e-45_f32),         // subnormal
                any::<f32>(),
            ]
        }

        fn arb_float64() -> impl Strategy<Value = f64> {
            prop_oneof![
                Just(0.0f64),
                Just(-0.0f64),
                Just(f64::NAN),
                Just(f64::INFINITY),
                Just(f64::NEG_INFINITY),
                Just(f64::MIN_POSITIVE),
                Just(5e-324_f64), // subnormal
                any::<f64>(),
            ]
        }

        fn arb_bigint() -> impl Strategy<Value = BigInt> {
            prop_oneof![
                // small values fitting i64
                any::<i64>().prop_map(BigInt::from),
                // large values NOT fitting i64
                any::<i128>()
                    .prop_filter("does not fit i64", |n| {
                        *n > i64::MAX as i128 || *n < i64::MIN as i128
                    })
                    .prop_map(BigInt::from),
                Just(BigInt::from(0)),
            ]
        }

        fn arb_decimal() -> impl Strategy<Value = bigdecimal::BigDecimal> {
            (any::<i64>(), 0i64..18)
                .prop_map(|(m, s)| bigdecimal::BigDecimal::new(BigInt::from(m), s))
        }

        /// Strategy for non-numeric, non-NaN Value variants (comparable types only).
        fn arb_non_numeric_comparable_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                ".*".prop_map(Value::Text),
                prop::collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
                arb_date().prop_map(Value::Date),
                arb_timestamp().prop_map(Value::Timestamp),
                any::<[u8; 16]>().prop_map(|b| Value::Uuid(uuid::Uuid::from_bytes(b))),
                any::<u32>().prop_map(|n| Value::Ipv4(std::net::Ipv4Addr::from(n.to_be_bytes()))),
                any::<[u8; 16]>().prop_map(|b| Value::Ipv6(std::net::Ipv6Addr::from(b))),
            ]
        }

        fn arb_date() -> impl Strategy<Value = chrono::NaiveDate> {
            (1970i32..2100, 1u32..=12, 1u32..=28)
                .prop_map(|(y, m, d)| chrono::NaiveDate::from_ymd_opt(y, m, d).expect("valid date"))
        }

        fn arb_timestamp() -> impl Strategy<Value = chrono::DateTime<chrono::Utc>> {
            any::<i64>().prop_filter_map("range", |seconds| {
                let s = seconds % 4_000_000_000;
                chrono::DateTime::<chrono::Utc>::from_timestamp(s, 0)
            })
        }

        /// Strategy for ALL non-Custom Value variants, including NaN floats.
        fn arb_any_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                arb_numeric_value(),
                arb_non_numeric_comparable_value(),
                any::<i64>().prop_map(|n| Value::Json(serde_json::json!({ "n": n }))),
            ]
        }

        /// Strategy for non-NaN Value variants (reflexivity-safe).
        fn arb_non_nan_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                any::<i8>().prop_map(Value::Int8),
                any::<i16>().prop_map(Value::Int16),
                any::<i32>().prop_map(Value::Int32),
                any::<i64>().prop_map(Value::Int64),
                any::<u8>().prop_map(Value::UInt8),
                any::<u16>().prop_map(Value::UInt16),
                any::<u32>().prop_map(Value::UInt32),
                any::<u64>().prop_map(Value::UInt64),
                any::<f32>()
                    .prop_filter("no NaN", |f| !f.is_nan())
                    .prop_map(Value::Float32),
                any::<f64>()
                    .prop_filter("no NaN", |f| !f.is_nan())
                    .prop_map(Value::Float64),
                arb_bigint().prop_map(Value::BigInt),
                arb_decimal().prop_map(Value::Decimal),
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                ".*".prop_map(Value::Text),
                prop::collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
                arb_date().prop_map(Value::Date),
                arb_timestamp().prop_map(Value::Timestamp),
                any::<[u8; 16]>().prop_map(|b| Value::Uuid(uuid::Uuid::from_bytes(b))),
                any::<u32>().prop_map(|n| Value::Ipv4(std::net::Ipv4Addr::from(n.to_be_bytes()))),
                any::<[u8; 16]>().prop_map(|b| Value::Ipv6(std::net::Ipv6Addr::from(b))),
                any::<i64>().prop_map(|n| Value::Json(serde_json::json!({ "n": n }))),
            ]
        }

        /// Strategy that produces a single i8 value wrapped in ALL integer widths.
        fn arb_int_widths_from_i8() -> impl Strategy<Value = (i8, Vec<Value>)> {
            any::<i8>().prop_map(|n| {
                let variants = vec![
                    Value::Int8(n),
                    Value::Int16(n as i16),
                    Value::Int32(n as i32),
                    Value::Int64(n as i64),
                    Value::BigInt(BigInt::from(n)),
                ];
                (n, variants)
            })
        }

        // ── 1. Reflexivity ────────────────────────────────────────────

        /// compare_values(&v, &v) == Some(Equal) for all non-NaN values.
        #[test_strategy::proptest(ProptestConfig::with_cases(512))]
        fn reflexivity_all_non_nan(#[strategy(arb_non_nan_value())] v: Value) {
            prop_assert_eq!(
                compare_values(&v, &v),
                Some(Ordering::Equal),
                "reflexivity violated for {:?}",
                v
            );
        }

        /// NaN is the only exception to reflexivity.
        #[test_strategy::proptest]
        fn reflexivity_nan_exception(#[strategy(arb_float64())] f: f64) {
            let v = Value::Float64(f);
            if f.is_nan() {
                prop_assert_eq!(
                    compare_values(&v, &v),
                    None,
                    "NaN should NOT be equal to itself"
                );
            } else {
                prop_assert_eq!(
                    compare_values(&v, &v),
                    Some(Ordering::Equal),
                    "non-NaN float64 must be reflexive"
                );
            }
        }

        #[test_strategy::proptest]
        fn reflexivity_nan_float32(#[strategy(arb_float32())] f: f32) {
            let v = Value::Float32(f);
            if f.is_nan() {
                prop_assert_eq!(
                    compare_values(&v, &v),
                    None,
                    "Float32 NaN should NOT be equal to itself"
                );
            } else {
                prop_assert_eq!(
                    compare_values(&v, &v),
                    Some(Ordering::Equal),
                    "non-NaN float32 must be reflexive"
                );
            }
        }

        // ── 2. Symmetry ──────────────────────────────────────────────

        /// If compare_values(&a, &b) == Some(X) then compare_values(&b, &a) == Some(X.reverse()).
        /// Tested across ALL value pairs, including cross-numeric.
        #[test_strategy::proptest(ProptestConfig::with_cases(1024))]
        fn symmetry_all_values(
            #[strategy(arb_any_value())] a: Value,
            #[strategy(arb_any_value())] b: Value,
        ) {
            let ab = compare_values(&a, &b);
            let ba = compare_values(&b, &a);
            match (ab, ba) {
                (Some(x), Some(y)) => {
                    prop_assert_eq!(
                        x,
                        y.reverse(),
                        "symmetry broken: compare({:?}, {:?}) = {:?}, \
                         reverse = {:?}",
                        a,
                        b,
                        x,
                        y
                    );
                }
                (None, None) => {} // both incompatible — OK
                _ => {
                    prop_assert!(
                        false,
                        "symmetry broken: one direction returned Some, the other None. \
                         compare({:?}, {:?}) = {:?}, compare reverse = {:?}",
                        a,
                        b,
                        ab,
                        ba
                    );
                }
            }
        }

        /// Symmetry specifically for cross-numeric pairs.
        #[test_strategy::proptest(ProptestConfig::with_cases(512))]
        fn symmetry_cross_numeric(
            #[strategy(arb_numeric_value())] a: Value,
            #[strategy(arb_numeric_value())] b: Value,
        ) {
            let ab = compare_values(&a, &b);
            let ba = compare_values(&b, &a);
            match (ab, ba) {
                (Some(x), Some(y)) => {
                    prop_assert_eq!(x, y.reverse());
                }
                (None, None) => {} // NaN cases
                _ => {
                    prop_assert!(
                        false,
                        "cross-numeric symmetry broken: \
                         compare({:?}, {:?}) = {:?}, compare reverse = {:?}",
                        a,
                        b,
                        ab,
                        ba
                    );
                }
            }
        }

        // ── 3. Transitivity ─────────────────────────────────────────

        /// If a < b and b < c then a < c. Tested with triples of mixed int widths.
        #[test_strategy::proptest(ProptestConfig::with_cases(512))]
        fn transitivity_mixed_ints(
            #[strategy(any::<i8>())] x: i8,
            #[strategy(any::<i8>())] y: i8,
            #[strategy(any::<i8>())] z: i8,
        ) {
            // Sort the raw values to get a guaranteed a <= b <= c chain
            let mut sorted = [x, y, z];
            sorted.sort();
            let [low, mid, high] = sorted;

            // Wrap in different widths to exercise cross-numeric promotion
            let a = Value::Int8(low);
            let b = Value::Int32(mid as i32);
            let c = Value::Int64(high as i64);

            let ab = compare_values(&a, &b);
            let bc = compare_values(&b, &c);
            let ac = compare_values(&a, &c);

            // If a < b and b < c, then a < c
            if ab == Some(Ordering::Less) && bc == Some(Ordering::Less) {
                prop_assert_eq!(
                    ac,
                    Some(Ordering::Less),
                    "transitivity broken: {:?} < {:?} and {:?} < {:?} but ac = {:?}",
                    a,
                    b,
                    b,
                    c,
                    ac
                );
            }
            // If a == b and b == c, then a == c
            if ab == Some(Ordering::Equal) && bc == Some(Ordering::Equal) {
                prop_assert_eq!(
                    ac,
                    Some(Ordering::Equal),
                    "transitivity broken for Equal: {:?} == {:?} == {:?} but ac = {:?}",
                    a,
                    b,
                    c,
                    ac
                );
            }
        }

        /// Transitivity with wider range: BigInt mixed with i64 and u64.
        #[test_strategy::proptest(ProptestConfig::with_cases(256))]
        fn transitivity_bigint_int64_uint64(
            #[strategy(any::<u32>())] x: u32,
            #[strategy(any::<u32>())] y: u32,
            #[strategy(any::<u32>())] z: u32,
        ) {
            let mut sorted = [x as i64, y as i64, z as i64];
            sorted.sort();
            let [low, mid, high] = sorted;

            let a = Value::Int64(low);
            let b = Value::UInt64(mid as u64);
            let c = Value::BigInt(BigInt::from(high));

            let ab = compare_values(&a, &b);
            let bc = compare_values(&b, &c);
            let ac = compare_values(&a, &c);

            if ab == Some(Ordering::Less) && bc == Some(Ordering::Less) {
                prop_assert_eq!(ac, Some(Ordering::Less));
            }
            if ab == Some(Ordering::Equal) && bc == Some(Ordering::Equal) {
                prop_assert_eq!(ac, Some(Ordering::Equal));
            }
        }

        // ── 4. PartialEq <-> PartialOrd consistency ─────────────────

        /// (a == b) iff a.partial_cmp(&b) == Some(Equal). Must hold for ALL value pairs.
        #[test_strategy::proptest(ProptestConfig::with_cases(1024))]
        fn partial_eq_iff_partial_cmp_equal(
            #[strategy(arb_any_value())] a: Value,
            #[strategy(arb_any_value())] b: Value,
        ) {
            let eq = a == b;
            let cmp_eq = a.partial_cmp(&b) == Some(Ordering::Equal);
            prop_assert_eq!(
                eq,
                cmp_eq,
                "PartialEq/PartialOrd inconsistency: ({:?} == {:?}) = {}, \
                 partial_cmp = {:?}",
                a,
                b,
                eq,
                a.partial_cmp(&b)
            );
        }

        /// Same as above but focused on cross-numeric pairs.
        #[test_strategy::proptest(ProptestConfig::with_cases(512))]
        fn partial_eq_iff_partial_cmp_equal_numeric(
            #[strategy(arb_numeric_value())] a: Value,
            #[strategy(arb_numeric_value())] b: Value,
        ) {
            let eq = a == b;
            let cmp_eq = a.partial_cmp(&b) == Some(Ordering::Equal);
            prop_assert_eq!(
                eq,
                cmp_eq,
                "numeric PartialEq/PartialOrd inconsistency: \
                 ({:?} == {:?}) = {}, partial_cmp = {:?}",
                a,
                b,
                eq,
                a.partial_cmp(&b)
            );
        }

        /// compare_values and Value::partial_cmp must agree.
        #[test_strategy::proptest(ProptestConfig::with_cases(512))]
        fn partial_ord_delegates_to_compare_values(
            #[strategy(arb_any_value())] a: Value,
            #[strategy(arb_any_value())] b: Value,
        ) {
            prop_assert_eq!(
                a.partial_cmp(&b),
                compare_values(&a, &b),
                "PartialOrd should delegate to compare_values"
            );
        }

        // ── 5. Cross-width integer identity ──────────────────────────

        /// For any i8 value n: Int8(n) == Int16(n) == Int32(n) == Int64(n) == BigInt(n).
        /// All pairs must compare Equal.
        #[test_strategy::proptest]
        fn cross_width_integer_identity(
            #[strategy(arb_int_widths_from_i8())] pair: (i8, Vec<Value>),
        ) {
            let (_n, variants) = pair;
            for i in 0..variants.len() {
                for j in 0..variants.len() {
                    prop_assert_eq!(
                        compare_values(&variants[i], &variants[j]),
                        Some(Ordering::Equal),
                        "cross-width mismatch: {:?} vs {:?}",
                        variants[i],
                        variants[j]
                    );
                    prop_assert!(
                        variants[i] == variants[j],
                        "PartialEq mismatch: {:?} vs {:?}",
                        variants[i],
                        variants[j]
                    );
                }
            }
        }

        /// UInt widths: u8 and u16 values must equal their Int64 counterparts.
        #[test_strategy::proptest]
        fn cross_width_unsigned_small(#[strategy(any::<u16>())] n: u16) {
            let as_u8_ok = n <= u8::MAX as u16;
            let variants: Vec<Value> = [
                Some(Value::UInt16(n)),
                Some(Value::Int64(n as i64)),
                Some(Value::UInt32(n as u32)),
                Some(Value::BigInt(BigInt::from(n))),
                if as_u8_ok {
                    Some(Value::UInt8(n as u8))
                } else {
                    None
                },
            ]
            .into_iter()
            .flatten()
            .collect();

            for i in 0..variants.len() {
                for j in 0..variants.len() {
                    prop_assert_eq!(
                        compare_values(&variants[i], &variants[j]),
                        Some(Ordering::Equal),
                        "unsigned cross-width mismatch: {:?} vs {:?}",
                        variants[i],
                        variants[j]
                    );
                }
            }
        }

        // ── 6. UInt64 boundary ──────────────────────────────────────

        /// UInt64(i64::MAX as u64) == Int64(i64::MAX).
        #[test_strategy::proptest]
        fn uint64_at_i64_max(#[strategy(Just(i64::MAX as u64))] n: u64) {
            let uint_val = Value::UInt64(n);
            let int_val = Value::Int64(i64::MAX);
            prop_assert_eq!(
                compare_values(&uint_val, &int_val),
                Some(Ordering::Equal),
                "UInt64(i64::MAX) should equal Int64(i64::MAX)"
            );
        }

        /// UInt64(i64::MAX + 1) promotes to BigInt and must compare > Int64(i64::MAX).
        #[test_strategy::proptest]
        fn uint64_above_i64_max(#[strategy((i64::MAX as u64 + 1)..=u64::MAX)] n: u64) {
            let uint_val = Value::UInt64(n);
            let int_max = Value::Int64(i64::MAX);

            let result = compare_values(&uint_val, &int_max);
            prop_assert_eq!(
                result,
                Some(Ordering::Greater),
                "UInt64({}) should be Greater than Int64(i64::MAX)",
                n
            );
        }

        /// UInt64 values beyond i64::MAX must compare correctly against their BigInt equivalent.
        #[test_strategy::proptest]
        fn uint64_equals_bigint(#[strategy(any::<u64>())] n: u64) {
            let uint_val = Value::UInt64(n);
            let big_val = Value::BigInt(BigInt::from(n));
            prop_assert_eq!(
                compare_values(&uint_val, &big_val),
                Some(Ordering::Equal),
                "UInt64({}) should equal BigInt({})",
                n,
                n
            );
        }

        // ── 7. Float promotion ──────────────────────────────────────

        /// Float32(x) and Float64(x as f64) must compare Equal for all finite f32.
        #[test_strategy::proptest]
        fn float32_promotes_to_float64(
            #[strategy(any::<f32>().prop_filter("finite", |f| f.is_finite()))] x: f32,
        ) {
            let f32_val = Value::Float32(x);
            let f64_val = Value::Float64(x as f64);
            prop_assert_eq!(
                compare_values(&f32_val, &f64_val),
                Some(Ordering::Equal),
                "Float32({}) should equal Float64({}) after promotion",
                x,
                x as f64,
            );
        }

        /// Float32 infinities must equal their Float64 counterparts.
        #[test]
        fn float32_infinity_promotes() {
            assert_eq!(
                compare_values(
                    &Value::Float32(f32::INFINITY),
                    &Value::Float64(f64::INFINITY)
                ),
                Some(Ordering::Equal)
            );
            assert_eq!(
                compare_values(
                    &Value::Float32(f32::NEG_INFINITY),
                    &Value::Float64(f64::NEG_INFINITY)
                ),
                Some(Ordering::Equal)
            );
        }

        /// Float32 NaN promoted to Float64 NaN must still return None.
        #[test]
        fn float32_nan_promotes_to_float64_nan() {
            assert_eq!(
                compare_values(&Value::Float32(f32::NAN), &Value::Float64(f64::NAN)),
                None
            );
            assert_eq!(
                compare_values(&Value::Float32(f32::NAN), &Value::Float64(1.0)),
                None
            );
        }

        // ── 8. IPv4 <-> IPv6 mapped ────────────────────────────────

        /// Ipv4(addr) == Ipv6(addr.to_ipv6_mapped()).
        #[test_strategy::proptest]
        fn ipv4_equals_ipv6_mapped(#[strategy(any::<u32>())] raw: u32) {
            let ipv4 = std::net::Ipv4Addr::from(raw.to_be_bytes());
            let ipv6_mapped = ipv4.to_ipv6_mapped();

            let v4 = Value::Ipv4(ipv4);
            let v6 = Value::Ipv6(ipv6_mapped);

            prop_assert_eq!(
                compare_values(&v4, &v6),
                Some(Ordering::Equal),
                "Ipv4({}) should equal Ipv6({})",
                ipv4,
                ipv6_mapped
            );
            // Symmetry
            prop_assert_eq!(
                compare_values(&v6, &v4),
                Some(Ordering::Equal),
                "Ipv6({}) should equal Ipv4({})",
                ipv6_mapped,
                ipv4
            );
        }

        /// Ipv4 vs non-mapped Ipv6 should produce consistent ordering.
        #[test_strategy::proptest]
        fn ipv4_vs_ipv6_symmetry(
            #[strategy(any::<u32>())] raw4: u32,
            #[strategy(any::<[u8; 16]>())] raw6: [u8; 16],
        ) {
            let v4 = Value::Ipv4(std::net::Ipv4Addr::from(raw4.to_be_bytes()));
            let v6 = Value::Ipv6(std::net::Ipv6Addr::from(raw6));

            let ab = compare_values(&v4, &v6);
            let ba = compare_values(&v6, &v4);
            match (ab, ba) {
                (Some(x), Some(y)) => {
                    prop_assert_eq!(x, y.reverse());
                }
                _ => prop_assert!(false, "IPv4 vs IPv6 should always produce Some ordering"),
            }
        }

        // ── 9. Incompatible types ───────────────────────────────────

        /// Text vs Int64 must return None, not panic.
        #[test_strategy::proptest]
        fn incompatible_text_vs_int64(
            #[strategy(".*")] s: String,
            #[strategy(any::<i64>())] n: i64,
        ) {
            let text = Value::Text(s);
            let int = Value::Int64(n);
            prop_assert_eq!(
                compare_values(&text, &int),
                None,
                "Text vs Int64 should return None"
            );
            prop_assert_eq!(
                compare_values(&int, &text),
                None,
                "Int64 vs Text should return None"
            );
        }

        /// Bool vs numeric returns None.
        #[test_strategy::proptest]
        fn incompatible_bool_vs_numeric(
            #[strategy(any::<bool>())] b: bool,
            #[strategy(arb_numeric_value())] n: Value,
        ) {
            let bool_val = Value::Bool(b);
            prop_assert_eq!(
                compare_values(&bool_val, &n),
                None,
                "Bool vs numeric should return None: Bool({}) vs {:?}",
                b,
                n
            );
        }

        /// Null vs any non-Null returns None.
        #[test_strategy::proptest(ProptestConfig::with_cases(256))]
        fn null_vs_non_null(#[strategy(arb_any_value())] v: Value) {
            if !v.is_null() {
                prop_assert_eq!(
                    compare_values(&Value::Null, &v),
                    None,
                    "Null vs {:?} should return None",
                    v
                );
                prop_assert_eq!(
                    compare_values(&v, &Value::Null),
                    None,
                    "{:?} vs Null should return None",
                    v
                );
            }
        }

        // ── Additional invariants ──────────────────────────────────

        /// Decimal equality: normalized vs non-normalized representations.
        /// BigDecimal(1.50, scale=2) should equal BigDecimal(1.5, scale=1)
        /// if Decimal comparison normalizes. If it does not, this test
        /// will detect broken equality.
        #[test]
        fn decimal_normalized_vs_unnormalized() {
            use std::str::FromStr;
            let a = Value::Decimal(bigdecimal::BigDecimal::from_str("1.50").unwrap());
            let b = Value::Decimal(bigdecimal::BigDecimal::from_str("1.5").unwrap());
            // BigDecimal's Ord normalizes, so these should be Equal
            assert_eq!(
                compare_values(&a, &b),
                Some(Ordering::Equal),
                "Decimal 1.50 vs 1.5 should be Equal"
            );
        }

        /// Positive zero and negative zero must compare Equal for floats.
        #[test]
        fn float_positive_and_negative_zero() {
            assert_eq!(
                compare_values(&Value::Float64(0.0), &Value::Float64(-0.0)),
                Some(Ordering::Equal)
            );
            assert_eq!(
                compare_values(&Value::Float32(0.0), &Value::Float32(-0.0)),
                Some(Ordering::Equal)
            );
        }

        /// Int64 ↔ Float64 precision boundary: large i64 values may lose
        /// precision when cast to f64. The comparison should still not panic.
        #[test_strategy::proptest]
        fn int64_vs_float64_no_panic(#[strategy(any::<i64>())] n: i64) {
            let int_val = Value::Int64(n);
            let float_val = Value::Float64(n as f64);
            // We do not assert a specific result here — the semantics of
            // lossy promotion are defined by the impl. We just verify no
            // panic and that symmetry holds.
            let ab = compare_values(&int_val, &float_val);
            let ba = compare_values(&float_val, &int_val);
            match (ab, ba) {
                (Some(x), Some(y)) => prop_assert_eq!(x, y.reverse()),
                (None, None) => {} // both NaN — OK
                _ => prop_assert!(false, "one-sided None for Int64({}) vs Float64", n),
            }
        }

        /// Json values: structural equality or None (not panic, not wrong ordering).
        #[test_strategy::proptest]
        fn json_equality_only(#[strategy(any::<i64>())] a: i64, #[strategy(any::<i64>())] b: i64) {
            let ja = Value::Json(serde_json::json!({ "v": a }));
            let jb = Value::Json(serde_json::json!({ "v": b }));
            let result = compare_values(&ja, &jb);
            if a == b {
                prop_assert_eq!(result, Some(Ordering::Equal));
            } else {
                prop_assert_eq!(
                    result,
                    None,
                    "unequal Json values must return None, not an ordering"
                );
            }
        }

        /// Existing starter tests preserved below.
        #[test_strategy::proptest]
        fn compare_reflexive(#[strategy(arb_i64())] n: i64) {
            let v = Value::Int64(n);
            assert_eq!(compare_values(&v, &v), Some(Ordering::Equal));
        }

        #[test_strategy::proptest]
        fn cross_numeric_int_widths(#[strategy(i8::MIN..=i8::MAX)] n: i8) {
            let v8 = Value::Int8(n);
            let v64 = Value::Int64(n as i64);
            assert!(values_equal(&v8, &v64));
            assert_eq!(compare_values(&v8, &v64), Some(Ordering::Equal));
        }
    }
}
