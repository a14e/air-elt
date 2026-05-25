use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::detect::{has_interpolation, is_expression};

/// A config value that may contain an expression.
/// Auto-detects at deserialization time whether a string is an expression,
/// contains interpolation, or is a plain literal.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ExprValue {
    /// Detected expression: string starts with name(...)
    Expression(String),
    /// String containing {interpolation} markers
    Interpolated(String),
    /// Plain TOML value (no expression detected)
    Literal(toml::Value),
}

impl<'de> Deserialize<'de> for ExprValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        Ok(match &value {
            toml::Value::String(s) if is_expression(s) => ExprValue::Expression(s.clone()),
            toml::Value::String(s) if has_interpolation(s) => ExprValue::Interpolated(s.clone()),
            _ => ExprValue::Literal(value),
        })
    }
}
