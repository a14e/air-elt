//! `BigInt → BigInt(width)` saturating to `±(10^width - 1)` and
//! `BigInt → Int*/UInt*` saturating to the target's range.

use super::error::ConvertError;
use super::saturate::*;
use crate::{DataType, Value};

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;
    let b = match value {
        Value::BigInt(b) => b,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    match dst {
        BigInt { width: Some(w) } => Ok(Value::BigInt(sat_bigint_to_width(&b, *w))),
        BigInt { width: None } => Ok(Value::BigInt(b)),
        Int64 => Ok(Value::Int64(sat_bigint_to_i64(&b))),
        Int32 => Ok(Value::Int32(sat_bigint_to_i32(&b))),
        Int16 => Ok(Value::Int16(sat_bigint_to_i16(&b))),
        Int8 => Ok(Value::Int8(sat_bigint_to_i8(&b))),
        UInt64 => Ok(Value::UInt64(sat_bigint_to_u64(&b))),
        UInt32 => Ok(Value::UInt32(sat_bigint_to_u32(&b))),
        UInt16 => Ok(Value::UInt16(sat_bigint_to_u16(&b))),
        UInt8 => Ok(Value::UInt8(sat_bigint_to_u8(&b))),
        _ => Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use num_bigint::BigInt;
    use std::str::FromStr;

    const SRC: DataType = DataType::BigInt { width: None };

    fn b(s: &str) -> Value {
        Value::BigInt(BigInt::from_str(s).unwrap())
    }

    #[test]
    fn bigint_to_bigint_unbounded_passthrough() {
        let out = convert(b("123"), &SRC, &DataType::BigInt { width: None }).unwrap();
        assert_eq!(out, b("123"));
    }

    #[test]
    fn bigint_to_bigint_width_saturates_positive() {
        let out = convert(
            b("99999999999"),
            &SRC,
            &DataType::BigInt { width: Some(10) },
        )
        .unwrap();
        assert_eq!(out, b("9999999999"));
    }

    #[test]
    fn bigint_to_bigint_width_saturates_negative() {
        let out = convert(
            b("-99999999999"),
            &SRC,
            &DataType::BigInt { width: Some(10) },
        )
        .unwrap();
        assert_eq!(out, b("-9999999999"));
    }

    #[test]
    fn bigint_to_signed_int_each_width_uses_width_specific_value() {
        // Per-arm meaningful inputs: each value is wider than the next-smaller
        // target, so a swapped arm would saturate to the wrong target's max.
        for (input, dst, expected) in [
            (b("12345"), DataType::Int16, Value::Int16(12_345)),
            (
                b("2000000000"),
                DataType::Int32,
                Value::Int32(2_000_000_000),
            ),
            (
                b("9000000000000000000"),
                DataType::Int64,
                Value::Int64(9_000_000_000_000_000_000),
            ),
        ] {
            assert_eq!(convert(input, &SRC, &dst).unwrap(), expected);
        }
    }

    #[test]
    fn bigint_huge_saturates_each_signed() {
        let huge = b("99999999999999999999999");
        for (dst, expected) in [
            (DataType::Int64, Value::Int64(i64::MAX)),
            (DataType::Int32, Value::Int32(i32::MAX)),
            (DataType::Int16, Value::Int16(i16::MAX)),
        ] {
            assert_eq!(convert(huge.clone(), &SRC, &dst).unwrap(), expected);
        }
    }

    #[test]
    fn bigint_negative_saturates_to_unsigned_zero() {
        let neg = b("-1");
        for (dst, expected) in [
            (DataType::UInt64, Value::UInt64(0)),
            (DataType::UInt32, Value::UInt32(0)),
            (DataType::UInt16, Value::UInt16(0)),
            (DataType::UInt8, Value::UInt8(0)),
        ] {
            assert_eq!(convert(neg.clone(), &SRC, &dst).unwrap(), expected);
        }
    }

    #[test]
    fn bigint_huge_saturates_each_unsigned() {
        let huge = b("99999999999999999999999");
        for (dst, expected) in [
            (DataType::UInt64, Value::UInt64(u64::MAX)),
            (DataType::UInt32, Value::UInt32(u32::MAX)),
            (DataType::UInt16, Value::UInt16(u16::MAX)),
            (DataType::UInt8, Value::UInt8(u8::MAX)),
        ] {
            assert_eq!(convert(huge.clone(), &SRC, &dst).unwrap(), expected);
        }
    }

    #[test]
    fn value_shape_mismatch() {
        let res = convert(Value::Int32(1), &SRC, &DataType::Int32);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn unsupported_target_rejected() {
        let res = convert(b("0"), &SRC, &DataType::Bool);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    // Build a `BigInt` whose magnitude is bounded but freely arbitrary —
    // `(any::<i128>() as base, 0..30u8 as scale_exp)` widens through
    // `base * 10^exp`, covering both within-width and outside-width
    // magnitudes.
    prop_compose! {
        fn arb_bigint()
            (base in any::<i128>(), exp in 0u8..30u8)
            -> BigInt
        {
            let mut value = BigInt::from(base);
            let ten = BigInt::from(10);
            for _ in 0..exp {
                value *= &ten;
            }
            value
        }
    }

    /// Any `BigInt` whose magnitude fits the target `width` (i.e.
    /// `|b| <= 10^width - 1`) must survive narrowing unchanged. We build
    /// `b` *inside* the width range by reducing modulo `(max + 1)` so
    /// generation never rejects.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn bigint_narrow_within_range_identity(
        #[strategy(arb_bigint())] raw: BigInt,
        #[strategy(1u32..=38u32)] width: u32,
    ) {
        // Compute 10^width - 1 inline (mirrors production logic).
        let mut max = BigInt::from(1);
        let ten = BigInt::from(10);
        for _ in 0..width {
            max *= &ten;
        }
        max -= 1;
        // Force into range without rejecting — preserve sign.
        let modulus = max.clone() + BigInt::from(1);
        let in_range = if raw.sign() == num_bigint::Sign::Minus {
            -((-&raw) % &modulus)
        } else {
            &raw % &modulus
        };
        let out = convert(
            Value::BigInt(in_range.clone()),
            &SRC,
            &DataType::BigInt { width: Some(width) },
        )
        .expect("convert");
        prop_assert_eq!(out, Value::BigInt(in_range));
    }

    /// Values outside `±(10^width - 1)` must saturate to that bound.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn bigint_narrow_outside_range_saturates(
        #[strategy(arb_bigint())] b: BigInt,
        #[strategy(1u32..=18u32)] width: u32,
    ) {
        use num_bigint::Sign;
        let mut max = BigInt::from(1);
        let ten = BigInt::from(10);
        for _ in 0..width {
            max *= &ten;
        }
        max -= 1;
        let neg_max = -max.clone();
        prop_assume!(b > max || b < neg_max);
        let out = convert(
            Value::BigInt(b.clone()),
            &SRC,
            &DataType::BigInt { width: Some(width) },
        )
        .expect("convert");
        let expected = match b.sign() {
            Sign::Minus => neg_max.clone(),
            _ => max.clone(),
        };
        prop_assert_eq!(out, Value::BigInt(expected));
    }

    /// `width = 0` is a valid (if degenerate) `BigInt(width)` slot —
    /// the saturating bound is `±(10^0 - 1) = 0`, so every input
    /// saturates to `BigInt(0)`. The dispatcher path was previously
    /// uncovered; this pins it independently from the
    /// `saturate::sat_bigint_to_width_invariants` primitive-level
    /// property.
    #[test]
    fn convert_with_width_zero_saturates_to_zero() {
        for input in [
            b("123"),
            b("-123"),
            b("0"),
            b("99999999999999999999"),
            b("-99999999999999999999"),
        ] {
            let out = convert(input.clone(), &SRC, &DataType::BigInt { width: Some(0) }).unwrap();
            assert_eq!(out, Value::BigInt(BigInt::from(0)), "for input {input:?}");
        }
    }

    /// `BigInt(-0)` and `BigInt(0)` are the same value (no signed zero in
    /// arbitrary-precision integers); the equality must be transitive
    /// across convert paths.
    #[test]
    fn bigint_negative_zero_equality() {
        let pos = BigInt::from(0);
        let neg = -BigInt::from(0);
        assert_eq!(pos, neg);
        // Round-trip through convert.
        let out = convert(
            Value::BigInt(neg.clone()),
            &SRC,
            &DataType::BigInt { width: Some(5) },
        )
        .unwrap();
        assert_eq!(out, Value::BigInt(BigInt::from(0)));
    }
}
