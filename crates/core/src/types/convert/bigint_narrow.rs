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
