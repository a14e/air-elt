use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static IS_NULL: IsNullFunc = IsNullFunc;
static IS_NOT_NULL: IsNotNullFunc = IsNotNullFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&IS_NULL);
    registry.register(&IS_NOT_NULL);
}

struct IsNullFunc;

impl ExprFunction for IsNullFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "isNull"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        Ok(Value::Bool(args.read(0).is_null()))
    }
}

struct IsNotNullFunc;

impl ExprFunction for IsNotNullFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "isNotNull"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        Ok(Value::Bool(!args.read(0).is_null()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn is_null_true() {
        let f = IsNullFunc;
        let result = eval(&f, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn is_null_false() {
        let f = IsNullFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn is_not_null() {
        let f = IsNotNullFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(true));
    }
}
