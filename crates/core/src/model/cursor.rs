use serde::{Deserialize, Serialize};

use crate::types::value::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorState {
    pub fields: Vec<CursorFieldValue>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CursorFieldValue {
    pub name: String,
    pub value: Value,
}

impl CursorState {
    pub fn new(fields: Vec<CursorFieldValue>) -> Self {
        Self { fields }
    }
}
