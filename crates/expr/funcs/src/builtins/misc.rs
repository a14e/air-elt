use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static TYPE_OF: TypeOfFunc = TypeOfFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&TYPE_OF);
}

struct TypeOfFunc;

impl ExprFunction for TypeOfFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "typeof"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        // Runtime type name comes from the canonical `Value::variant_name`
        // (PascalCase tag, or a `Custom` value's real `DynType::kind()`),
        // so `typeof` never re-derives the variant match locally.
        Ok(Value::Text(args.read(0).variant_name()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn typeof_int64() {
        let f = TypeOfFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Int64".into()));
    }

    #[test]
    fn typeof_text() {
        let f = TypeOfFunc;
        let result = eval(&f, smallvec::smallvec![Value::Text("hi".into())], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Text".into()));
    }

    #[test]
    fn typeof_null() {
        let f = TypeOfFunc;
        let result = eval(&f, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Null".into()));
    }

    #[test]
    fn typeof_bool() {
        let f = TypeOfFunc;
        let result = eval(&f, smallvec::smallvec![Value::Bool(true)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Bool".into()));
    }

    #[test]
    fn typeof_float64() {
        let f = TypeOfFunc;
        let result = eval(&f, smallvec::smallvec![Value::Float64(1.5)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Float64".into()));
    }
}
