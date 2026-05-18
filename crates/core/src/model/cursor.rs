use serde::{Deserialize, Serialize};

use crate::error::{RuntimeError, RuntimeResult};
use crate::types::DataType;
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

    /// Decode a previously-serialised [`CursorState`] JSON using the
    /// supplied per-field types as the dispatch key for
    /// [`DataType::decode_cursor_json`]. `cursor_types` is in the same
    /// order as the cursor fields the state was originally saved
    /// against (`ReadSpec.cursor_fields`). For canonical (non-Custom)
    /// types the dispatch boils down to `serde_json::from_value`; for
    /// `DataType::Custom(t)` the typed entry point unwraps the
    /// `{type,kind,value}` envelope and asks `t.decode_cursor_value`
    /// to recover the concrete `DynValue` — no global registry is
    /// involved.
    pub fn from_typed_json(
        json: serde_json::Value,
        cursor_types: &[DataType],
    ) -> RuntimeResult<Self> {
        let obj = json.as_object().ok_or_else(|| {
            RuntimeError::Other(format!("expected CursorState object envelope, got {json}",))
        })?;
        let raw_fields = obj
            .get("fields")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                RuntimeError::Other("CursorState JSON missing array field `fields`".to_string())
            })?;
        if raw_fields.len() != cursor_types.len() {
            return Err(RuntimeError::Other(format!(
                "CursorState JSON has {} fields but caller supplied {} expected types",
                raw_fields.len(),
                cursor_types.len()
            )));
        }
        let mut fields = Vec::with_capacity(raw_fields.len());
        for (raw, expected) in raw_fields.iter().zip(cursor_types.iter()) {
            let entry = raw.as_object().ok_or_else(|| {
                RuntimeError::Other(format!(
                    "CursorState field entry must be an object, got {raw}",
                ))
            })?;
            let name = entry
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    RuntimeError::Other("CursorState field missing string `name`".to_string())
                })?
                .to_string();
            let value_json = entry
                .get("value")
                .cloned()
                .ok_or_else(|| RuntimeError::Other("CursorState field missing `value`".into()))?;
            let value = expected
                .decode_cursor_json(value_json)
                .map_err(RuntimeError::Other)?;
            fields.push(CursorFieldValue { name, value });
        }
        Ok(Self { fields })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Happy path: a valid cursor envelope with two heterogeneous
    /// fields decodes to a fully-populated [`CursorState`].
    #[test]
    fn from_typed_json_happy_path_int64_and_text() {
        let envelope = serde_json::json!({
            "fields": [
                { "name": "id", "value": { "type": "int64", "value": 42 } },
                { "name": "tag", "value": { "type": "text", "value": "alpha" } },
            ],
        });
        let cursor_types = [DataType::Int64, DataType::Text { size: None }];
        let state = CursorState::from_typed_json(envelope, &cursor_types).expect("decode succeeds");
        let expected = CursorState::new(vec![
            CursorFieldValue {
                name: "id".into(),
                value: Value::Int64(42),
            },
            CursorFieldValue {
                name: "tag".into(),
                value: Value::Text("alpha".into()),
            },
        ]);
        assert_eq!(state, expected);
    }

    /// Envelope carries one field, caller declares two expected types.
    /// Must surface as a `RuntimeError::Other` whose message names both
    /// counts so the operator can spot the schema drift.
    #[test]
    fn from_typed_json_length_mismatch_errors() {
        let envelope = serde_json::json!({
            "fields": [
                { "name": "id", "value": { "type": "int64", "value": 1 } },
            ],
        });
        let cursor_types = [DataType::Int64, DataType::Int64];
        let err = CursorState::from_typed_json(envelope, &cursor_types)
            .expect_err("length mismatch must error");
        match err {
            RuntimeError::Other(msg) => {
                assert!(
                    msg.contains("1 fields") && msg.contains("2 expected"),
                    "unexpected message: {msg}"
                );
            }
            other => panic!("expected RuntimeError::Other, got {other:?}"),
        }
    }

    /// Top-level object without a `fields` key — the envelope is
    /// structurally malformed.
    #[test]
    fn from_typed_json_missing_fields_key_errors() {
        let envelope = serde_json::json!({
            "other": [],
        });
        let cursor_types = [DataType::Int64];
        let err = CursorState::from_typed_json(envelope, &cursor_types)
            .expect_err("missing `fields` must error");
        match err {
            RuntimeError::Other(msg) => assert!(
                msg.contains("missing array field `fields`"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected RuntimeError::Other, got {other:?}"),
        }
    }

    /// Field entry is an object but lacks the `value` key.
    #[test]
    fn from_typed_json_missing_value_key_errors() {
        let envelope = serde_json::json!({
            "fields": [
                { "name": "id" },
            ],
        });
        let cursor_types = [DataType::Int64];
        let err = CursorState::from_typed_json(envelope, &cursor_types)
            .expect_err("missing `value` must error");
        match err {
            RuntimeError::Other(msg) => {
                assert!(msg.contains("missing `value`"), "unexpected message: {msg}")
            }
            other => panic!("expected RuntimeError::Other, got {other:?}"),
        }
    }

    /// Top-level JSON is a literal, not an object — covers the very
    /// first `as_object()` guard.
    #[test]
    fn from_typed_json_non_object_envelope_errors() {
        let envelope = serde_json::json!(42);
        let cursor_types: [DataType; 0] = [];
        let err = CursorState::from_typed_json(envelope, &cursor_types)
            .expect_err("non-object envelope must error");
        match err {
            RuntimeError::Other(msg) => assert!(
                msg.contains("expected CursorState object envelope"),
                "unexpected message: {msg}"
            ),
            other => panic!("expected RuntimeError::Other, got {other:?}"),
        }
    }
}
