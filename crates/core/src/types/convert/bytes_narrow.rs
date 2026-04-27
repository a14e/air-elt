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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn value_shape_mismatch() {
        let res = convert(Value::Int32(1), &DataType::Bytes { size: None }, None);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn passthrough_when_size_none() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3, 4, 5]),
            &DataType::Bytes { size: None },
            None,
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3, 4, 5]));
    }

    #[test]
    fn truncates_to_max() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3, 4, 5]),
            &DataType::Bytes { size: Some(3) },
            Some(3),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn max_zero_yields_empty() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3]),
            &DataType::Bytes { size: Some(0) },
            Some(0),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![]));
    }

    #[test]
    fn exact_fit_passthrough() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3]),
            &DataType::Bytes { size: Some(3) },
            Some(3),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3]));
    }
}
