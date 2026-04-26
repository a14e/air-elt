//! Saturating integer narrowing under `truncate=true`.
//!
//! Covers signed → smaller signed, unsigned → smaller unsigned, signed →
//! unsigned (negative → 0), and unsigned → signed. All routes saturate to
//! the target's representable range — no wrapping, never panics. Identity
//! and pure widening are handled by the dispatcher's short-circuit.

use super::error::ConvertError;
use super::saturate::*;
use crate::types::{DataType, Value};

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;

    // Lift the source into the widest signed/unsigned canonical form, then
    // saturate down to the target. Only the (src, value) variants that
    // actually exist for that DataType are accepted.
    match (src, dst) {
        // Signed → smaller signed.
        (Int64, Int32) => match value {
            Value::Int64(n) => Ok(Value::Int32(sat_i64_to_i32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, Int16) => match value {
            Value::Int64(n) => Ok(Value::Int16(sat_i64_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, Int16) => match value {
            Value::Int32(n) => Ok(Value::Int16(sat_i32_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → smaller unsigned.
        (UInt64, UInt32) => match value {
            Value::UInt64(n) => Ok(Value::UInt32(sat_u64_to_u32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, UInt16) => match value {
            Value::UInt64(n) => Ok(Value::UInt16(sat_u64_to_u16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, UInt8) => match value {
            Value::UInt64(n) => Ok(Value::UInt8(sat_u64_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, UInt16) => match value {
            Value::UInt32(n) => Ok(Value::UInt16(sat_u32_to_u16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, UInt8) => match value {
            Value::UInt32(n) => Ok(Value::UInt8(sat_u32_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt16, UInt8) => match value {
            Value::UInt16(n) => Ok(Value::UInt8(sat_u16_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Signed → unsigned (negatives → 0, then saturate).
        (Int64, UInt64) => match value {
            Value::Int64(n) => Ok(Value::UInt64(sat_i64_to_u64(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, UInt32) => match value {
            Value::Int64(n) => Ok(Value::UInt32(sat_i64_to_u32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, UInt16) => match value {
            Value::Int64(n) => Ok(Value::UInt16(sat_i64_to_u16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, UInt8) => match value {
            Value::Int64(n) => Ok(Value::UInt8(sat_i64_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt64) => match value {
            Value::Int32(n) => Ok(Value::UInt64(sat_i64_to_u64(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt32) => match value {
            Value::Int32(n) => Ok(Value::UInt32(sat_i64_to_u32(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt16) => match value {
            Value::Int32(n) => Ok(Value::UInt16(sat_i64_to_u16(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt8) => match value {
            Value::Int32(n) => Ok(Value::UInt8(sat_i64_to_u8(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt64) => match value {
            Value::Int16(n) => Ok(Value::UInt64(sat_i64_to_u64(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt32) => match value {
            Value::Int16(n) => Ok(Value::UInt32(sat_i64_to_u32(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt16) => match value {
            Value::Int16(n) => Ok(Value::UInt16(sat_i64_to_u16(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt8) => match value {
            Value::Int16(n) => Ok(Value::UInt8(sat_i64_to_u8(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → signed (saturate at signed max).
        (UInt64, Int64) => match value {
            Value::UInt64(n) => Ok(Value::Int64(sat_u64_to_i64(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, Int32) => match value {
            Value::UInt64(n) => Ok(Value::Int32(sat_u64_to_i32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, Int16) => match value {
            Value::UInt64(n) => Ok(Value::Int16(sat_u64_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, Int32) => match value {
            Value::UInt32(n) => Ok(Value::Int32(sat_u32_to_i32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, Int16) => match value {
            Value::UInt32(n) => Ok(Value::Int16(sat_u32_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt16, Int16) => match value {
            Value::UInt16(n) => Ok(Value::Int16(sat_u16_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        _ => Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
        }),
    }
}
