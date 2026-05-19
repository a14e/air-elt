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
//! ≤ 7 for Float32) lives in `core::types::matrix`; this module only
//! implements the conversion itself.

use super::error::ConvertError;
use crate::types::{DataType, Value};
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
}
