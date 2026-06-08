use air_elt_types::{DataType, Value};

use crate::error::FuncError;

pub fn extract_text(val: Value, func_name: &str) -> Result<String, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

pub fn extract_text_ref<'a>(val: &'a Value, func_name: &str) -> Result<&'a str, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

pub fn extract_bytes(val: Value, func_name: &str) -> Result<Vec<u8>, FuncError> {
    match val {
        Value::Bytes(b) => Ok(b),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Bytes".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

pub fn extract_int64(val: Value, func_name: &str) -> Result<i64, FuncError> {
    match val {
        Value::Int64(n) => Ok(n),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Int64".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

pub fn validate_text_arg(function: &str, dt: &DataType) -> Result<(), FuncError> {
    if !matches!(dt, DataType::Text { .. }) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{dt}"),
        });
    }
    Ok(())
}
