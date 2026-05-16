//! `build_body_json`: assembles a `serde_json::Value::Object` from per-cell
//! canonical [`Value`]s paired with column names.
//!
//! Used by relational sources (postgres, mysql) when the flow has a body
//! target — the source attaches the resulting JSON wrapped in
//! `Value::Json(...)` on `Row.body`. The Transform interpreter
//! `Body` op then absorbs the value.

use crate::error::RuntimeResult;
use crate::types::{Value, value_to_json};

/// Build a `serde_json::Value::Object` from per-cell `Value`s paired
/// with column names.
pub fn build_body_json(
    values: &[Value],
    column_names: &[String],
) -> RuntimeResult<serde_json::Value> {
    if values.len() != column_names.len() {
        return Err(crate::error::RuntimeError::DerivedPlanInvariant {
            detail: format!(
                "build_body_json: values.len {} != column_names.len {}",
                values.len(),
                column_names.len()
            ),
        });
    }
    let mut map = serde_json::Map::with_capacity(values.len());
    for (name, value) in column_names.iter().zip(values.iter()) {
        map.insert(name.clone(), value_to_json(value)?);
    }
    Ok(serde_json::Value::Object(map))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn build_body_json_basic() {
        let v = build_body_json(
            &[Value::Int32(1), Value::Text("x".into())],
            &["a".to_string(), "b".to_string()],
        )
        .unwrap();
        assert_eq!(v, serde_json::json!({"a": 1, "b": "x"}));
    }

    #[test]
    fn build_body_json_length_mismatch_errors() {
        let res = build_body_json(&[Value::Int32(1)], &[]);
        assert!(res.is_err());
    }

    #[test]
    fn build_body_json_handles_null_and_decimal() {
        use bigdecimal::BigDecimal;
        let v = build_body_json(
            &[Value::Null, Value::Decimal(BigDecimal::from(123))],
            &["a".to_string(), "b".to_string()],
        )
        .unwrap();
        assert_eq!(v, serde_json::json!({"a": null, "b": "123"}));
    }
}
