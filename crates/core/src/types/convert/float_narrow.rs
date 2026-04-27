//! `Float64 → Float32` (saturate to f32::MAX/MIN, NaN preserved) and
//! `Float64 → Int*` (truncate-toward-zero, then saturate). NaN → integer is
//! rejected as `Overflow`.

use super::error::ConvertError;
use super::saturate::*;
use crate::types::{DataType, Value};

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;
    let n = match (src, &value) {
        (Float64, Value::Float64(n)) => *n,
        (Float64, _) => return Err(ConvertError::ValueShapeMismatch { src: *src }),
        _ => {
            return Err(ConvertError::Unsupported {
                src: *src,
                dst: *dst,
            });
        }
    };

    match dst {
        Float32 => Ok(Value::Float32(sat_f64_to_f32(n))),
        Int64 => sat_f64_to_i64(n)
            .map(Value::Int64)
            .ok_or(ConvertError::Overflow { dst: *dst }),
        Int32 => sat_f64_to_i32(n)
            .map(Value::Int32)
            .ok_or(ConvertError::Overflow { dst: *dst }),
        Int16 => sat_f64_to_i16(n)
            .map(Value::Int16)
            .ok_or(ConvertError::Overflow { dst: *dst }),
        UInt64 => sat_f64_to_u64(n)
            .map(Value::UInt64)
            .ok_or(ConvertError::Overflow { dst: *dst }),
        UInt32 => sat_f64_to_u32(n)
            .map(Value::UInt32)
            .ok_or(ConvertError::Overflow { dst: *dst }),
        UInt16 => sat_f64_to_u16(n)
            .map(Value::UInt16)
            .ok_or(ConvertError::Overflow { dst: *dst }),
        UInt8 => sat_f64_to_u8(n)
            .map(Value::UInt8)
            .ok_or(ConvertError::Overflow { dst: *dst }),
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

    #[test]
    fn f64_to_f32_truncates_toward_zero_or_saturates() {
        assert_eq!(
            convert(Value::Float64(1.5), &DataType::Float64, &DataType::Float32).unwrap(),
            Value::Float32(1.5)
        );
        assert_eq!(
            convert(
                Value::Float64(f64::MAX),
                &DataType::Float64,
                &DataType::Float32
            )
            .unwrap(),
            Value::Float32(f32::MAX)
        );
    }

    #[test]
    fn f64_to_signed_ints_each_width() {
        for (dst, expected) in [
            (DataType::Int64, Value::Int64(7)),
            (DataType::Int32, Value::Int32(7)),
            (DataType::Int16, Value::Int16(7)),
        ] {
            let out = convert(Value::Float64(7.9), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn f64_to_unsigned_ints_each_width() {
        for (dst, expected) in [
            (DataType::UInt64, Value::UInt64(7)),
            (DataType::UInt32, Value::UInt32(7)),
            (DataType::UInt16, Value::UInt16(7)),
            (DataType::UInt8, Value::UInt8(7)),
        ] {
            let out = convert(Value::Float64(7.9), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn f64_negative_to_unsigned_clamps_to_zero() {
        // Pin the exact target variant — a swapped arm that produced
        // `UInt64(0)` for a `UInt8` request would otherwise pass.
        for (dst, expected) in [
            (DataType::UInt64, Value::UInt64(0)),
            (DataType::UInt32, Value::UInt32(0)),
            (DataType::UInt16, Value::UInt16(0)),
            (DataType::UInt8, Value::UInt8(0)),
        ] {
            let out = convert(Value::Float64(-1.5), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected, "dst={dst:?}");
        }
    }

    #[test]
    fn f64_per_width_overflow_saturates() {
        // An input that fits Int32 but overflows Int16, etc. — proves each
        // arm dispatches to its own width-specific saturating primitive.
        for (input, dst, expected) in [
            (40_000.0_f64, DataType::Int16, Value::Int16(i16::MAX)),
            (1e10_f64, DataType::Int32, Value::Int32(i32::MAX)),
            (70_000.0_f64, DataType::UInt16, Value::UInt16(u16::MAX)),
            (1e10_f64, DataType::UInt32, Value::UInt32(u32::MAX)),
            (300.0_f64, DataType::UInt8, Value::UInt8(u8::MAX)),
        ] {
            let out = convert(Value::Float64(input), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected, "input {input} → {dst:?}");
        }
    }

    #[test]
    fn nan_overflows_for_every_int_target() {
        for dst in [
            DataType::Int64,
            DataType::Int32,
            DataType::Int16,
            DataType::UInt64,
            DataType::UInt32,
            DataType::UInt16,
            DataType::UInt8,
        ] {
            let res = convert(Value::Float64(f64::NAN), &DataType::Float64, &dst);
            assert!(matches!(res, Err(ConvertError::Overflow { .. })), "{dst:?}");
        }
    }

    #[test]
    fn infinity_saturates_to_max() {
        let out = convert(
            Value::Float64(f64::INFINITY),
            &DataType::Float64,
            &DataType::Int64,
        )
        .unwrap();
        assert_eq!(out, Value::Int64(i64::MAX));
        let out = convert(
            Value::Float64(f64::NEG_INFINITY),
            &DataType::Float64,
            &DataType::Int64,
        )
        .unwrap();
        assert_eq!(out, Value::Int64(i64::MIN));
    }

    #[test]
    fn value_shape_mismatch_rejected() {
        let res = convert(Value::Int32(1), &DataType::Float64, &DataType::Int32);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn non_float64_source_rejected() {
        let res = convert(Value::Float32(1.0), &DataType::Float32, &DataType::Float32);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn unsupported_target_rejected() {
        let res = convert(Value::Float64(1.0), &DataType::Float64, &DataType::Bool);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }
}
