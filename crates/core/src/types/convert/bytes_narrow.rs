//! `Bytes → Bytes` size-narrowing under `truncate=true`. Hard byte cut, no
//! UTF-8 awareness.

use super::error::ConvertError;
use crate::types::{DataType, Value};

pub fn convert(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let mut bytes = match value {
        Value::Bytes(b) => b,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
    };
    if let Some(max) = sink_size
        && bytes.len() > max as usize
    {
        bytes.truncate(max as usize);
    }
    Ok(Value::Bytes(bytes))
}
