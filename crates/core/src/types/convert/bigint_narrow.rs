//! `BigInt → BigInt(width)` saturating to `±(10^width - 1)` and
//! `BigInt → Int*/UInt*` saturating to the target's range.

use super::error::ConvertError;
use super::saturate::*;
use crate::types::{DataType, Value};

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;
    let b = match value {
        Value::BigInt(b) => b,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
    };
    match dst {
        BigInt { width: Some(w) } => Ok(Value::BigInt(sat_bigint_to_width(&b, *w))),
        BigInt { width: None } => Ok(Value::BigInt(b)),
        Int64 => Ok(Value::Int64(sat_bigint_to_i64(&b))),
        Int32 => Ok(Value::Int32(sat_bigint_to_i32(&b))),
        Int16 => Ok(Value::Int16(sat_bigint_to_i16(&b))),
        UInt64 => Ok(Value::UInt64(sat_bigint_to_u64(&b))),
        UInt32 => Ok(Value::UInt32(sat_bigint_to_u32(&b))),
        UInt16 => Ok(Value::UInt16(sat_bigint_to_u16(&b))),
        UInt8 => Ok(Value::UInt8(sat_bigint_to_u8(&b))),
        _ => Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
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
}
