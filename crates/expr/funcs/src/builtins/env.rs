use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static ENV_ONE_ARG: EnvOneArgFunc = EnvOneArgFunc;
static ENV_TWO_ARG: EnvTwoArgFunc = EnvTwoArgFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&ENV_ONE_ARG);
    registry.register(&ENV_TWO_ARG);
}

/// `env("KEY")` - returns Null if not found
struct EnvOneArgFunc;

impl ExprFunction for EnvOneArgFunc {
    fn name(&self) -> &str {
        "env"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("env", &args[0].data_type)?;
        Ok(NullableExprType::nullable(DataType::Text { size: None }))
    }

    fn evaluate(&self, mut args: Vec<Value>, context: &EvalContext) -> Result<Value, FuncError> {
        let key = args.remove(0);
        if key.is_null() {
            return Ok(Value::Null);
        }
        let key_str = match key {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "env".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match context.env_resolver.get(&key_str) {
            Some(val) => Ok(Value::Text(val)),
            None => Ok(Value::Null),
        }
    }
}

/// `env("KEY", "default")` - returns default if not found
struct EnvTwoArgFunc;

impl ExprFunction for EnvTwoArgFunc {
    fn name(&self) -> &str {
        "env"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("env", &args[0].data_type)?;
        validate_text_arg("env", &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn evaluate(&self, mut args: Vec<Value>, context: &EvalContext) -> Result<Value, FuncError> {
        let default = args.remove(1);
        let key = args.remove(0);
        if key.is_null() {
            return Ok(default);
        }
        let key_str = match key {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "env".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match context.env_resolver.get(&key_str) {
            Some(val) => Ok(Value::Text(val)),
            None => Ok(default),
        }
    }
}

fn validate_text_arg(function: &str, dt: &DataType) -> Result<(), FuncError> {
    if !matches!(dt, DataType::Text { .. }) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{dt}"),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::signature::{EnvResolver, EvalContext};
    use std::path::PathBuf;

    struct TestEnv;

    impl EnvResolver for TestEnv {
        fn get(&self, key: &str) -> Option<String> {
            match key {
                "HOME" => Some("/home/user".to_owned()),
                _ => None,
            }
        }
    }

    fn ctx_with_env() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(TestEnv),
            file_resolver: Arc::new(crate::tests::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn env_found() {
        let f = EnvOneArgFunc;
        let result = f
            .evaluate(vec![Value::Text("HOME".into())], &ctx_with_env())
            .unwrap();
        assert_eq!(result, Value::Text("/home/user".into()));
    }

    #[test]
    fn env_not_found_returns_null() {
        let f = EnvOneArgFunc;
        let result = f
            .evaluate(vec![Value::Text("MISSING".into())], &ctx_with_env())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn env_with_default_found() {
        let f = EnvTwoArgFunc;
        let result = f
            .evaluate(
                vec![Value::Text("HOME".into()), Value::Text("fallback".into())],
                &ctx_with_env(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("/home/user".into()));
    }

    #[test]
    fn env_with_default_not_found() {
        let f = EnvTwoArgFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Text("MISSING".into()),
                    Value::Text("fallback".into()),
                ],
                &ctx_with_env(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("fallback".into()));
    }

    #[test]
    fn env_null_key() {
        let f = EnvOneArgFunc;
        let result = f.evaluate(vec![Value::Null], &ctx_with_env()).unwrap();
        assert_eq!(result, Value::Null);
    }
}
