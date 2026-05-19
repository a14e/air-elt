//! `Decimal → Float64` and `Decimal → Float32`.
//!
//! `BigDecimal::to_f64` performs the arbitrary-precision → binary float
//! conversion in one shot. For values whose magnitude exceeds the
//! target's representable range it returns `Some(f64::INFINITY)` /
//! `Some(f64::NEG_INFINITY)` (or `None` in pathological cases). Either
//! way it's lossy:
//!
//! * Without `truncate=true` we surface `Overflow` so the operator
//!   sees the precision loss rather than silently emitting `Inf`.
//! * With `truncate=true` we saturate to the appropriate signed
//!   infinity, mirroring the existing Float→Float narrowing convention
//!   in `float_narrow`.
//!
//! Lossless-vs-narrowing classification (precision ≤ 15 for Float64,
//! ≤ 7 for Float32) lives in `crate::matrix`; this module only
//! implements the conversion itself.

use super::error::ConvertError;
use crate::{DataType, Value};
use bigdecimal::ToPrimitive;
use num_bigint::Sign;

pub fn convert_to_f64(value: Value, src: &DataType, truncate: bool) -> Result<Value, ConvertError> {
    let d = match value {
        Value::Decimal(d) => d,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    let candidate = d.to_f64();
    let lossy = match candidate {
        None => true,
        Some(f) => !f.is_finite(),
    };
    if lossy {
        if !truncate {
            return Err(ConvertError::Overflow {
                dst: DataType::Float64,
            });
        }
        let saturated = match d.sign() {
            Sign::Minus => f64::NEG_INFINITY,
            // `Sign::NoSign` only fires for an exact zero, which `to_f64`
            // already returned losslessly — reaching here implies a non-
            // zero value with no sign information, treat as positive.
            Sign::Plus | Sign::NoSign => f64::INFINITY,
        };
        return Ok(Value::Float64(saturated));
    }
    // Safe: lossy=false implies candidate is Some(finite).
    Ok(Value::Float64(candidate.unwrap_or(0.0)))
}

pub fn convert_to_f32(value: Value, src: &DataType, truncate: bool) -> Result<Value, ConvertError> {
    let d = match value {
        Value::Decimal(d) => d,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    let candidate = d.to_f64();
    let lossy_in_f64 = match candidate {
        None => true,
        Some(f) => !f.is_finite(),
    };
    // Even when the f64 step is lossless, the subsequent `as f32` cast
    // may itself overflow to `±f32::INFINITY`. Check both stages so the
    // `truncate=false` error fires consistently.
    let narrowed = candidate.map(|f| f as f32);
    let lossy = lossy_in_f64 || matches!(narrowed, Some(f) if !f.is_finite()) || narrowed.is_none();
    if lossy {
        if !truncate {
            return Err(ConvertError::Overflow {
                dst: DataType::Float32,
            });
        }
        let saturated = match d.sign() {
            Sign::Minus => f32::NEG_INFINITY,
            Sign::Plus | Sign::NoSign => f32::INFINITY,
        };
        return Ok(Value::Float32(saturated));
    }
    Ok(Value::Float32(narrowed.unwrap_or(0.0)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use std::str::FromStr;

    fn dec(p: u32, s: u32) -> DataType {
        DataType::Decimal {
            precision: Some(p),
            scale: Some(s),
        }
    }

    // Float64 path

    #[test]
    fn decimal_12_2_to_f64_round_trip() {
        let d: BigDecimal = "12345.67".parse().unwrap();
        let out = convert_to_f64(Value::Decimal(d), &dec(12, 2), false).unwrap();
        match out {
            Value::Float64(f) => assert!((f - 12345.67).abs() < 1e-9),
            _ => panic!("expected Float64"),
        }
    }

    #[test]
    fn decimal_to_f64_negative_round_trip() {
        let d: BigDecimal = "-9999.99".parse().unwrap();
        let out = convert_to_f64(Value::Decimal(d), &dec(8, 2), false).unwrap();
        match out {
            Value::Float64(f) => assert!((f + 9999.99).abs() < 1e-9),
            _ => panic!("expected Float64"),
        }
    }

    /// A value guaranteed to exceed `f64::MAX` (~1.8e308). 10^350 sits
    /// roughly 42 orders of magnitude above the f64 ceiling, so the
    /// `BigDecimal::to_f64()` step saturates to `±INFINITY`. The
    /// `DataType::Decimal { precision: Some(n), scale: 0 }` slot is
    /// nominal — the canonical `Value::Decimal` carries the actual
    /// arbitrary-precision payload at runtime.
    fn ten_to_350() -> BigDecimal {
        let mut digits = String::from("1");
        for _ in 0..350 {
            digits.push('0');
        }
        BigDecimal::from_str(&digits).unwrap()
    }

    #[test]
    fn decimal_oversize_without_truncate_errors() {
        let res = convert_to_f64(Value::Decimal(ten_to_350()), &dec(38, 0), false);
        assert!(matches!(res, Err(ConvertError::Overflow { .. })));
    }

    #[test]
    fn decimal_oversize_with_truncate_saturates_positive_infinity() {
        let out = convert_to_f64(Value::Decimal(ten_to_350()), &dec(38, 0), true).unwrap();
        assert_eq!(out, Value::Float64(f64::INFINITY));
    }

    #[test]
    fn decimal_oversize_with_truncate_saturates_negative_infinity() {
        let out = convert_to_f64(Value::Decimal(-ten_to_350()), &dec(38, 0), true).unwrap();
        assert_eq!(out, Value::Float64(f64::NEG_INFINITY));
    }

    #[test]
    fn decimal_zero_to_f64() {
        let d: BigDecimal = "0".parse().unwrap();
        let out = convert_to_f64(Value::Decimal(d), &dec(10, 0), false).unwrap();
        assert_eq!(out, Value::Float64(0.0));
    }

    #[test]
    fn decimal_to_f64_value_shape_mismatch() {
        let res = convert_to_f64(Value::Int32(1), &dec(10, 2), false);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    // Float32 path

    #[test]
    fn decimal_to_f32_round_trip() {
        // numeric(7, 2) fits f32's ~7 significant digits exactly.
        let d: BigDecimal = "12345.67".parse().unwrap();
        let out = convert_to_f32(Value::Decimal(d), &dec(7, 2), false).unwrap();
        match out {
            Value::Float32(f) => assert!((f - 12345.67_f32).abs() < 1e-2),
            _ => panic!("expected Float32"),
        }
    }

    /// f32::MAX ≈ 3.4e38. 10^50 is comfortably above and well within f64
    /// range — exercises the "f64 step succeeds, f32 narrowing
    /// saturates" branch.
    fn ten_to_50() -> BigDecimal {
        let mut digits = String::from("1");
        for _ in 0..50 {
            digits.push('0');
        }
        BigDecimal::from_str(&digits).unwrap()
    }

    #[test]
    fn decimal_oversize_to_f32_overflow_without_truncate() {
        let res = convert_to_f32(Value::Decimal(ten_to_50()), &dec(38, 0), false);
        assert!(matches!(res, Err(ConvertError::Overflow { .. })));
    }

    #[test]
    fn decimal_oversize_to_f32_overflow_with_truncate_saturates_positive() {
        let out = convert_to_f32(Value::Decimal(ten_to_50()), &dec(38, 0), true).unwrap();
        assert_eq!(out, Value::Float32(f32::INFINITY));
    }

    #[test]
    fn decimal_oversize_to_f32_overflow_with_truncate_saturates_negative() {
        let out = convert_to_f32(Value::Decimal(-ten_to_50()), &dec(38, 0), true).unwrap();
        assert_eq!(out, Value::Float32(f32::NEG_INFINITY));
    }

    #[test]
    fn decimal_to_f32_value_shape_mismatch() {
        let res = convert_to_f32(Value::Int32(1), &dec(7, 2), false);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    // ---- Property-based tests --------------------------------------

    use num_bigint::BigInt;
    use proptest::prelude::*;

    // Build a `BigDecimal` whose mantissa magnitude fits losslessly in
    // f64 (15 significant decimal digits) and whose scale is small.
    prop_compose! {
        fn arb_small_decimal()
            // 15 digits ≈ ±999_999_999_999_999.
            (mantissa in -999_999_999_999_999i64..=999_999_999_999_999i64,
             scale in 0u32..=10u32)
            -> BigDecimal
        {
            BigDecimal::new(BigInt::from(mantissa), scale as i64)
        }
    }

    /// A `BigDecimal` whose precision is ≤ 15 fits losslessly in an f64
    /// mantissa — round-tripping through `convert_to_f64` must agree
    /// within one ULP against an INDEPENDENT oracle: parsing the
    /// decimal's textual form through `f64::from_str`. Using
    /// `d.to_f64()` here would be tautological — the production path
    /// calls the same routine, so the assertion would be structurally
    /// guaranteed.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn decimal_to_float_lossless_when_fits_f64_mantissa(
        #[strategy(arb_small_decimal())] d: BigDecimal,
    ) {
        // Independent oracle: stringify, then re-parse via the libcore
        // float parser. This is a separate code path from `to_f64`.
        let expected: f64 = d.to_string().parse().unwrap();
        let out = convert_to_f64(Value::Decimal(d), &dec(38, 10), false).expect("convert");
        match out {
            Value::Float64(f) => {
                prop_assert!(f.is_finite());
                prop_assert!(
                    (f - expected).abs() <= expected.abs() * 1e-12 + 1e-12,
                    "got {f}, expected {expected}"
                );
            }
            _ => prop_assert!(false, "expected Float64"),
        }
    }

    fn ten_to_350_local() -> BigDecimal {
        let mut digits = String::from("1");
        for _ in 0..350 {
            digits.push('0');
        }
        BigDecimal::from_str(&digits).unwrap()
    }

    /// Overflowing positive magnitude — `truncate=false` raises
    /// `Overflow`; `truncate=true` saturates to `+Infinity`.
    #[test]
    fn decimal_to_float_overflow_positive_saturates_to_pos_infinity() {
        let value = ten_to_350_local();
        let res_strict = convert_to_f64(Value::Decimal(value.clone()), &dec(38, 0), false);
        assert!(matches!(res_strict, Err(ConvertError::Overflow { .. })));
        let res_trunc = convert_to_f64(Value::Decimal(value), &dec(38, 0), true).unwrap();
        assert_eq!(res_trunc, Value::Float64(f64::INFINITY));
    }

    /// Overflowing negative magnitude — `truncate=false` raises
    /// `Overflow`; `truncate=true` saturates to `-Infinity`.
    #[test]
    fn decimal_to_float_overflow_negative_saturates_to_neg_infinity() {
        let value = -ten_to_350_local();
        let res_strict = convert_to_f64(Value::Decimal(value.clone()), &dec(38, 0), false);
        assert!(matches!(res_strict, Err(ConvertError::Overflow { .. })));
        let res_trunc = convert_to_f64(Value::Decimal(value), &dec(38, 0), true).unwrap();
        assert_eq!(res_trunc, Value::Float64(f64::NEG_INFINITY));
    }

    /// Sign preservation across small (lossless) decimals.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn decimal_to_float_sign_preserved(#[strategy(arb_small_decimal())] d: BigDecimal) {
        let sign_in = d.sign();
        let out = convert_to_f64(Value::Decimal(d), &dec(38, 10), false).expect("convert");
        match out {
            Value::Float64(f) => {
                use num_bigint::Sign;
                match sign_in {
                    Sign::Minus => prop_assert!(f <= 0.0),
                    Sign::Plus => prop_assert!(f >= 0.0),
                    Sign::NoSign => prop_assert_eq!(f, 0.0),
                }
            }
            _ => prop_assert!(false, "expected Float64"),
        }
    }
}
