//! `Text → Bool` conversion arm. The case-insensitive lexer itself lives in
//! [`utils::parse_bool`](super::utils::parse_bool); this wraps it into the
//! dispatch contract — accepted token → [`Value::Bool`], anything else →
//! [`ConvertError::InvalidBool`].

use super::error::ConvertError;
use super::utils::parse_bool;
use crate::{DataType, Value};

pub fn convert(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    match parse_bool(&s) {
        Some(b) => Ok(Value::Bool(b)),
        None => Err(ConvertError::InvalidBool { value: s }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn convert_value_shape_mismatch() {
        let res = convert(Value::Int32(1), &DataType::Text { size: None });
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn convert_returns_invalid_bool_for_unknown_token() {
        let res = convert(Value::Text("maybe".into()), &DataType::Text { size: None });
        assert!(matches!(res, Err(ConvertError::InvalidBool { .. })));
    }

    #[test]
    fn convert_returns_bool_for_truthy_text() {
        let out = convert(Value::Text("yes".into()), &DataType::Text { size: None }).unwrap();
        assert_eq!(out, Value::Bool(true));
    }

    #[test]
    fn convert_returns_bool_for_falsy_text() {
        let out = convert(Value::Text("0".into()), &DataType::Text { size: None }).unwrap();
        assert_eq!(out, Value::Bool(false));
    }
}
