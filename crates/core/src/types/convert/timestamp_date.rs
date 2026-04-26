//! `Timestamp → Date` under `truncate=true`. Drops the time-of-day part of
//! the UTC timestamp.

use super::error::ConvertError;
use crate::types::{DataType, Value};

pub fn convert(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    match value {
        Value::Timestamp(ts) => Ok(Value::Date(ts.date_naive())),
        _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
    }
}
