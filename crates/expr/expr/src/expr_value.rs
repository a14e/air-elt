use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_types::Value;
use serde::de::Deserializer;
use serde::{Deserialize, Serialize};

use crate::detect::{has_interpolation, is_expression};
use crate::error::ExprError;

/// A config value that may contain an expression.
/// Auto-detects at deserialization/parse time whether a string is an expression,
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

impl ExprValue {
    /// Parse a string into an ExprValue, detecting expression/interpolation/literal.
    pub fn parse(input: &str) -> Self {
        if is_expression(input) {
            Self::Expression(input.to_owned())
        } else if has_interpolation(input) {
            Self::Interpolated(input.to_owned())
        } else {
            Self::Literal(toml::Value::String(input.to_owned()))
        }
    }

    /// Parse a TOML value into an ExprValue.
    pub fn from_toml(value: toml::Value) -> Self {
        match &value {
            toml::Value::String(s) if is_expression(s) => Self::Expression(s.clone()),
            toml::Value::String(s) if has_interpolation(s) => Self::Interpolated(s.clone()),
            _ => Self::Literal(value),
        }
    }

    /// Evaluate this value using the given registry and context.
    /// - Expression → full eval
    /// - Interpolated → interpolation eval (returns Text)
    /// - Literal → convert TOML value to Value directly
    pub fn eval(
        &self,
        registry: &FunctionRegistry,
        context: &EvalContext,
    ) -> Result<Value, ExprError> {
        match self {
            Self::Expression(src) => crate::evaluator::eval_expression(src, registry, context),
            Self::Interpolated(src) => {
                let text = crate::evaluator::eval_interpolated(src, registry, context)?;
                Ok(Value::Text(text))
            }
            Self::Literal(toml_val) => Ok(toml_to_value(toml_val)),
        }
    }

    /// Returns true if this value requires expression evaluation.
    pub fn needs_eval(&self) -> bool {
        !matches!(self, Self::Literal(_))
    }
}

fn toml_to_value(val: &toml::Value) -> Value {
    match val {
        toml::Value::String(s) => Value::Text(s.clone()),
        toml::Value::Integer(n) => Value::Int64(*n),
        toml::Value::Float(f) => Value::Float64(*f),
        toml::Value::Boolean(b) => Value::Bool(*b),
        _ => Value::Text(val.to_string()),
    }
}

impl<'de> Deserialize<'de> for ExprValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = toml::Value::deserialize(deserializer)?;
        Ok(Self::from_toml(value))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
    use air_elt_expr_funcs::{FuncError, FunctionRegistry};
    use air_elt_types::Value;

    use super::*;

    struct EmptyEnv;
    impl EnvResolver for EmptyEnv {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
    }

    struct NoopFiles;
    impl FileResolver for NoopFiles {
        fn read(&self, path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
            Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: "noop".to_owned(),
            })
        }
    }

    fn ctx() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(EmptyEnv),
            file_resolver: Arc::new(NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn parse_expression() {
        let v = ExprValue::parse("concat('a', 'b')");
        assert!(matches!(v, ExprValue::Expression(_)));
    }

    #[test]
    fn parse_interpolation() {
        let v = ExprValue::parse("hello {1 + 1} world");
        assert!(matches!(v, ExprValue::Interpolated(_)));
    }

    #[test]
    fn parse_literal() {
        let v = ExprValue::parse("just a string");
        assert!(matches!(v, ExprValue::Literal(_)));
    }

    #[test]
    fn eval_expression() {
        let v = ExprValue::parse("add(1, 2)");
        let registry = FunctionRegistry::with_builtins();
        let result = v.eval(&registry, &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn eval_interpolation() {
        let v = ExprValue::parse("result: {1 + 1}");
        let registry = FunctionRegistry::with_builtins();
        let result = v.eval(&registry, &ctx()).unwrap();
        assert_eq!(result, Value::Text("result: 2".to_owned()));
    }

    #[test]
    fn eval_literal_string() {
        let v = ExprValue::parse("plain text");
        let registry = FunctionRegistry::with_builtins();
        let result = v.eval(&registry, &ctx()).unwrap();
        assert_eq!(result, Value::Text("plain text".to_owned()));
    }

    #[test]
    fn from_toml_integer() {
        let v = ExprValue::from_toml(toml::Value::Integer(42));
        let registry = FunctionRegistry::with_builtins();
        let result = v.eval(&registry, &ctx()).unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn from_toml_expression_string() {
        let v = ExprValue::from_toml(toml::Value::String("add(10, 20)".into()));
        let registry = FunctionRegistry::with_builtins();
        let result = v.eval(&registry, &ctx()).unwrap();
        assert_eq!(result, Value::Int64(30));
    }

    #[test]
    fn needs_eval_false_for_literal() {
        let v = ExprValue::parse("hello");
        assert!(!v.needs_eval());
    }

    #[test]
    fn needs_eval_true_for_expression() {
        let v = ExprValue::parse("concat('a', 'b')");
        assert!(v.needs_eval());
    }
}
