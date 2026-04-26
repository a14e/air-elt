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
