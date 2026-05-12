//! `Decimal → Decimal(p, s)` (drop scale + saturate precision),
//! `Decimal → BigInt(width)` (drop scale + saturate width), and
//! `Decimal → Int*/UInt*` (drop scale + saturate range). All routes
//! truncate toward zero; over-range integer parts saturate at min/max.

use super::error::ConvertError;
use super::saturate::*;
use crate::types::{DataType, Value};
use bigdecimal::BigDecimal;

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;
    let d = match value {
        Value::Decimal(d) => d,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    match dst {
        Decimal { precision, scale } => Ok(Value::Decimal(narrow_decimal(d, *precision, *scale))),
        BigInt { width } => {
            let b = bigdecimal_to_bigint_truncating(&d);
            let saturated = match width {
                Some(w) => sat_bigint_to_width(&b, *w),
                None => b,
            };
            Ok(Value::BigInt(saturated))
        }
        Int64 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int64(sat_bigint_to_i64(&b)))
        }
        Int32 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int32(sat_bigint_to_i32(&b)))
        }
        Int16 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int16(sat_bigint_to_i16(&b)))
        }
        Int8 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int8(sat_bigint_to_i8(&b)))
        }
        UInt64 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt64(sat_bigint_to_u64(&b)))
        }
        UInt32 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt32(sat_bigint_to_u32(&b)))
        }
        UInt16 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt16(sat_bigint_to_u16(&b)))
        }
        UInt8 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt8(sat_bigint_to_u8(&b)))
        }
        _ => Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        }),
    }
}

fn narrow_decimal(d: BigDecimal, precision: Option<u32>, scale: Option<u32>) -> BigDecimal {
    // Determine effective target scale. Per the type matrix, `Decimal { Some(p),
    // None }` is canonicalised to `scale = 0` (precision applies to the integer
    // part only). Fully unbounded `(None, None)` keeps the source as-is.
    let target_scale = match (precision, scale) {
        (None, None) => return d,
        (_, Some(s)) => s as i64,
        (Some(_), None) => 0,
    };
    let mut out = d.with_scale(target_scale);
    if let Some(p) = precision {
        let (mantissa, mantissa_scale) = out.into_bigint_and_exponent();
        // sat_bigint_to_width caps |mantissa| < 10^p — the BigDecimal's
        // underlying invariant after `with_scale(s)` aligns with this.
        let saturated = sat_bigint_to_width(&mantissa, p);
        out = BigDecimal::new(saturated, mantissa_scale);
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn dec(p: u32, s: u32) -> DataType {
        DataType::Decimal {
            precision: Some(p),
            scale: Some(s),
        }
    }

    #[test]
    fn narrow_decimal_drops_fractional_when_scale_none() {
        // Decimal { Some(p), None } is canonicalised to scale=0 — fractional
        // part must be truncated toward zero.
        let d: BigDecimal = "12.99".parse().unwrap();
        let out = narrow_decimal(d, Some(5), None);
        assert_eq!(out, BigDecimal::from(12));
    }

    #[test]
    fn narrow_decimal_preserves_when_fully_unbounded() {
        let d: BigDecimal = "12.99".parse().unwrap();
        let out = narrow_decimal(d.clone(), None, None);
        assert_eq!(out, d);
    }

    #[test]
    fn narrow_decimal_truncates_scale_then_caps_precision() {
        let d: BigDecimal = "123.456".parse().unwrap();
        // p=4, s=2 → integer-digits=2, max representable 99.99.
        let out = narrow_decimal(d, Some(4), Some(2));
        // mantissa-cap saturates to 9999 with scale 2 → 99.99.
        assert_eq!(out, BigDecimal::from_str("99.99").unwrap());
    }

    #[test]
    fn convert_value_shape_mismatch() {
        let res = convert(Value::Int32(1), &dec(10, 2), &DataType::Int32);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn convert_unsupported_target() {
        let d: BigDecimal = "1".parse().unwrap();
        let res = convert(Value::Decimal(d), &dec(10, 2), &DataType::Bool);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn decimal_to_unbounded_bigint_no_saturation() {
        let d: BigDecimal = "12.99".parse().unwrap();
        let res = convert(
            Value::Decimal(d),
            &dec(10, 2),
            &DataType::BigInt { width: None },
        )
        .unwrap();
        assert_eq!(res, Value::BigInt(num_bigint::BigInt::from(12)));
    }

    /// All target arms share the `Value::Decimal` extraction at the top —
    /// passing a wrong-variant value through any of them must surface
    /// `ValueShapeMismatch`, not a panic.
    #[test]
    fn value_shape_mismatch_on_every_target() {
        let dsts = [
            DataType::Decimal {
                precision: Some(10),
                scale: Some(2),
            },
            DataType::BigInt { width: None },
            DataType::Int64,
            DataType::Int32,
            DataType::Int16,
            DataType::UInt64,
            DataType::UInt32,
            DataType::UInt16,
            DataType::UInt8,
        ];
        for dst in dsts {
            let res = convert(Value::Bool(true), &dec(10, 2), &dst);
            assert!(
                matches!(res, Err(ConvertError::ValueShapeMismatch { .. })),
                "expected ValueShapeMismatch for dst={dst:?}, got {res:?}"
            );
        }
    }

    #[test]
    fn decimal_to_bigint_with_width_saturates() {
        let d: BigDecimal = "9999999999.99".parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dec(20, 2),
            &DataType::BigInt { width: Some(5) },
        )
        .unwrap();
        match out {
            Value::BigInt(b) => assert_eq!(b.to_string(), "99999"),
            _ => panic!("expected BigInt"),
        }
    }
}
