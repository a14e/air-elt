use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static AND: AndFunc = AndFunc;
static OR: OrFunc = OrFunc;
static NOT: NotFunc = NotFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&AND);
    registry.register(&OR);
    registry.register(&NOT);
}

struct AndFunc;

impl ExprFunction for AndFunc {
    fn name(&self) -> &str {
        "and"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Bool(x), Value::Bool(y)) => Ok(Value::Bool(x && y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "and".to_owned(),
                expected: "Bool".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct OrFunc;

impl ExprFunction for OrFunc {
    fn name(&self) -> &str {
        "or"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Bool(x), Value::Bool(y)) => Ok(Value::Bool(x || y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "or".to_owned(),
                expected: "Bool".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct NotFunc;

impl ExprFunction for NotFunc {
    fn name(&self) -> &str {
        "not"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Bool, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Bool(x) => Ok(Value::Bool(!x)),
            other => Err(FuncError::TypeMismatch {
                function: "not".to_owned(),
                expected: "Bool".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::signature::EvalContext;
    use std::path::PathBuf;

    fn ctx() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(crate::tests::EmptyEnv),
            file_resolver: Arc::new(crate::tests::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn and_true_true() {
        let f = AndFunc;
        let result = f
            .evaluate(vec![Value::Bool(true), Value::Bool(true)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn and_true_false() {
        let f = AndFunc;
        let result = f
            .evaluate(vec![Value::Bool(true), Value::Bool(false)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn or_false_true() {
        let f = OrFunc;
        let result = f
            .evaluate(vec![Value::Bool(false), Value::Bool(true)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn not_true() {
        let f = NotFunc;
        let result = f.evaluate(vec![Value::Bool(true)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn null_propagation() {
        let f = AndFunc;
        let result = f
            .evaluate(vec![Value::Null, Value::Bool(true)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }
}
