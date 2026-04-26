//! `Json → Text(size)`: serialize JSON to its compact string form, then
//! UTF-safe-truncate to the sink's declared size. `Json → Text*` (unbounded
//! sink) is the same minus the truncate.

use super::error::ConvertError;
use super::truncate_utf8::truncate_utf8;
use crate::types::{DataType, Value};

pub fn convert(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let v = match value {
        Value::Json(v) => v,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
    };
    // serde_json::Value cannot contain non-finite f64 (Number::from_f64
    // rejects NaN/±Inf at construction time), so to_string is infallible.
    let serialized =
        serde_json::to_string(&v).expect("serde_json::Value always serializes successfully");
    let out = match sink_size {
        None => serialized,
        Some(max) => truncate_utf8(&serialized, max as usize).to_string(),
    };
    Ok(Value::Text(out))
}
