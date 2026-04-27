//! `Json → Text(size)`: serialize JSON to its compact string form, then
//! truncate to the sink's declared size in **characters**. `Json → Text*`
//! (unbounded sink) is the same minus the truncate.

use super::error::ConvertError;
use super::text_truncate::truncate_chars;
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
        Some(max) => truncate_chars(&serialized, max as usize).to_string(),
    };
    Ok(Value::Text(out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn value_shape_mismatch() {
        let res = convert(Value::Text("abc".into()), &DataType::Json, None);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn serializes_array() {
        let v = serde_json::json!([1, 2, 3]);
        let out = convert(Value::Json(v), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("[1,2,3]".into()));
    }

    #[test]
    fn serializes_nested_object() {
        let v = serde_json::json!({"a": {"b": [1]}});
        let out = convert(Value::Json(v), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("{\"a\":{\"b\":[1]}}".into()));
    }

    #[test]
    fn serializes_null_payload() {
        let out = convert(Value::Json(serde_json::Value::Null), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("null".into()));
    }

    #[test]
    fn truncates_bounded_sink_in_chars() {
        // Object serializes to `{"a":1,"b":2}` (13 chars). Truncate to 5.
        let v = serde_json::json!({"a": 1, "b": 2});
        let out = convert(Value::Json(v), &DataType::Json, Some(5)).unwrap();
        assert_eq!(out, Value::Text("{\"a\":".into()));
    }

    #[test]
    fn unbounded_passthrough() {
        let v = serde_json::json!({"x": "hello"});
        let out = convert(Value::Json(v), &DataType::Json, None).unwrap();
        assert_eq!(out, Value::Text("{\"x\":\"hello\"}".into()));
    }
}
