//! XML ↔ Text conversions.
//!
//! `Xml → Text(size)` is a pure serialize (XML is already a string) followed
//! by UTF-safe truncation when the sink is bounded. `Xml → Text*`
//! (unbounded) is allowed without `truncate=true` — no truncation needed.
//! `Text → Xml` validates well-formedness via `quick-xml::Reader` to keep
//! invalid markup out of typed sink columns.

use super::error::ConvertError;
use super::truncate_utf8::truncate_utf8;
use crate::types::{DataType, Value};

pub fn xml_to_text(
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

pub fn text_to_xml(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
    };
    validate_well_formed(&s)?;
    Ok(Value::Text(s))
}

fn validate_well_formed(s: &str) -> Result<(), ConvertError> {
    super::xml_validate::validate(s).map_err(|reason| ConvertError::InvalidXml { reason })
}
