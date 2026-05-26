use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static NOT: NotFunc = NotFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&NOT);
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
    use super::*;
    use crate::test_support::ctx;

    #[test]
    fn not_true() {
        let f = NotFunc;
        let result = f.evaluate(vec![Value::Bool(true)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn not_null_propagation() {
        let f = NotFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }
}
