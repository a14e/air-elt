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

        #[test_strategy::proptest]
        fn compare_antisymmetric(#[strategy(arb_i64())] a: i64, #[strategy(arb_i64())] b: i64) {
            let va = Value::Int64(a);
            let vb = Value::Int64(b);
            if let (Some(ab), Some(ba)) = (compare_values(&va, &vb), compare_values(&vb, &va)) {
                assert_eq!(ab, ba.reverse());
            }
        }

        #[test_strategy::proptest]
        fn partial_eq_consistent_with_compare(
            #[strategy(arb_i64())] a: i64,
            #[strategy(arb_i64())] b: i64,
        ) {
            let va = Value::Int64(a);
            let vb = Value::Int64(b);
            let eq = va == vb;
            let cmp_eq = compare_values(&va, &vb) == Some(Ordering::Equal);
            assert_eq!(eq, cmp_eq);
        }

        #[test_strategy::proptest]
        fn partial_ord_consistent_with_compare(
            #[strategy(arb_i64())] a: i64,
            #[strategy(arb_i64())] b: i64,
        ) {
            let va = Value::Int64(a);
            let vb = Value::Int64(b);
            assert_eq!(va.partial_cmp(&vb), compare_values(&va, &vb));
        }
    }
}
