use serde::{Deserialize, Serialize};

use crate::types::value::Value;

/// Persisted cursor: ordered list of (field_name, last_value) pairs.
///
/// The semantics are "strictly greater than" — i.e. the source reads rows
/// where `(cursor_fields) > (values)` in the configured order.
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

    /// True if any cursor field is NULL — callers use this to decide whether
    /// the source should emit null-aware SQL for the strict-greater comparison
    /// (the common all-non-null path stays on plain tuple compare).
    pub fn has_null(&self) -> bool {
        self.fields.iter().any(|f| matches!(f.value, Value::Null))
    }
}
