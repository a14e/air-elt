//! `Text → Text` size-narrowing under `truncate=true`. Identity / pure
//! widening is handled by the dispatcher's short-circuit and never reaches
//! this module.

use super::error::ConvertError;
use super::truncate_utf8::truncate_utf8;
use crate::types::{DataType, Value};

pub fn convert(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
    };
    let out = match sink_size {
        None => s,
        Some(max) => truncate_utf8(&s, max as usize).to_string(),
    };
    Ok(Value::Text(out))
}
