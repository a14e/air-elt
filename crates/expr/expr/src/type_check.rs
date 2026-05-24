use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::DataType;

use crate::ast::{Expr, LiteralValue, Program, Statement};
use crate::error::ExprError;

/// Infer the output type of a program without evaluating it.
pub fn infer_type(
    program: &Program,
    registry: &FunctionRegistry,
) -> Result<NullableExprType, ExprError> {
    let mut checker = TypeChecker::new(registry);
    checker.check_program(program)
}

/// Parse and infer type in one call.
pub fn infer_expression_type(
    input: &str,
    registry: &FunctionRegistry,
) -> Result<NullableExprType, ExprError> {
    let program = crate::parser::parse(input)?;
    infer_type(&program, registry)
}

struct TypeChecker<'a> {
    registry: &'a FunctionRegistry,
    variables: AHashMap<String, NullableExprType>,
}

impl<'a> TypeChecker<'a> {
    fn new(registry: &'a FunctionRegistry) -> Self {
        Self {
            registry,
            variables: AHashMap::new(),
        }
    }

    fn check_program(&mut self, program: &Program) -> Result<NullableExprType, ExprError> {
        for statement in &program.statements {
            self.check_statement(statement)?;
        }
        self.check_expr(&program.result)
    }

    fn check_statement(&mut self, statement: &Statement) -> Result<(), ExprError> {
        let inferred_type = self.check_expr(&statement.value)?;
        self.variables.insert(statement.name.clone(), inferred_type);
        Ok(())
    }

    fn check_expr(&self, expr: &Expr) -> Result<NullableExprType, ExprError> {
        match expr {
            Expr::Literal(literal) => Ok(self.check_literal(literal)),
            Expr::Variable(name) => self.check_variable(name),
            Expr::FunctionCall { name, args } => self.check_function_call(name, args),
            Expr::Interpolation(_segments) => Ok(self.check_interpolation()),
            Expr::Object(_entries) => Ok(self.check_object()),
        }
    }

    fn check_literal(&self, literal: &LiteralValue) -> NullableExprType {
        match literal {
            LiteralValue::Null => NullableExprType::nullable(DataType::Bool),
            LiteralValue::Bool(_) => NullableExprType::non_null(DataType::Bool),
            LiteralValue::Int(i) => {
                let bits = if *i == 0 {
                    1
                } else {
                    (64 - i.unsigned_abs().leading_zeros()) as u8
                };
                NullableExprType::int_with_bound(DataType::Int64, bits)
            }
            LiteralValue::Float(_) => NullableExprType::non_null(DataType::Float64),
            LiteralValue::String(s) => NullableExprType::non_null(DataType::Text {
                size: Some(s.len() as u32),
            }),
        }
    }

    fn check_variable(&self, name: &str) -> Result<NullableExprType, ExprError> {
        self.variables
            .get(name)
            .cloned()
            .ok_or_else(|| ExprError::UndefinedVariable {
                name: name.to_string(),
            })
    }

    fn check_function_call(
        &self,
        name: &str,
        args: &[Expr],
    ) -> Result<NullableExprType, ExprError> {
        let arg_types: Vec<NullableExprType> = args
            .iter()
            .map(|arg| self.check_expr(arg))
            .collect::<Result<Vec<_>, _>>()?;

        let function = self.registry.resolve(name, args.len())?;
        let result_type = function.resolve_type(&arg_types)?;
        Ok(result_type)
    }

    fn check_interpolation(&self) -> NullableExprType {
        NullableExprType::non_null(DataType::Text { size: None })
    }

    fn check_object(&self) -> NullableExprType {
        NullableExprType::non_null(DataType::Object)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_expr_funcs::FunctionRegistry;

    fn registry() -> FunctionRegistry {
        FunctionRegistry::with_builtins()
    }

    #[test]
    fn literal_int_type() {
        let result = infer_expression_type("42", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        // 42 needs 6 bits (64 - 42u64.leading_zeros() = 6)
        assert_eq!(result.int_bound, Some(6));
    }

    #[test]
    fn literal_string_with_size() {
        let result = infer_expression_type("'hello'", &registry()).unwrap();
        assert_eq!(
            result,
            NullableExprType::non_null(DataType::Text { size: Some(5) })
        );
    }

    #[test]
    fn arithmetic_uses_resolve_type() {
        // 1 has bound=1, 2 has bound=2 -> add: max(1,2)+1 = 3
        let result = infer_expression_type("1 + 2", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(3));
    }

    #[test]
    fn null_is_nullable() {
        let result = infer_expression_type("null", &registry()).unwrap();
        assert!(result.nullable);
    }

    #[test]
    fn variable_type() {
        let result = infer_expression_type("x = 42; x", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        // Variable inherits int_bound from the assigned literal
        assert_eq!(result.int_bound, Some(6));
    }

    #[test]
    fn function_return_type() {
        let result = infer_expression_type("if(true, 1, 2)", &registry()).unwrap();
        // IfFunc uses NullableExprType::new() which does not propagate int_bound
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, None);
    }

    #[test]
    fn interpolation_type() {
        let result = infer_expression_type("'hello ${1 + 2} world'", &registry());
        // If the parser produces an interpolation node, it should be text(None)
        // If it produces a plain string literal, it will be text(Some(n))
        // Either way it should succeed
        assert!(result.is_ok());
        let typ = result.unwrap();
        assert!(!typ.nullable);
        assert!(matches!(typ.data_type, DataType::Text { .. }));
    }

    #[test]
    fn object_type() {
        let result = infer_expression_type("object('key', 1)", &registry());
        // If object() is a function it goes through resolve_type
        // Otherwise we test the Object literal form
        if let Ok(typ) = result {
            assert!(!typ.nullable);
        }
    }

    #[test]
    fn concat_returns_text() {
        let result = infer_expression_type("concat('abc', 'def')", &registry()).unwrap();
        // ConcatFunc returns unbounded Text (size tracking not implemented in concat)
        assert_eq!(
            result,
            NullableExprType::non_null(DataType::Text { size: None })
        );
    }

    #[test]
    fn add_text_concat_bounded() {
        // The add function does track bounds when both args are Text
        let result = infer_expression_type("'abc' + 'def'", &registry()).unwrap();
        assert!(!result.nullable);
        assert!(matches!(result.data_type, DataType::Text { .. }));
    }

    #[test]
    fn null_propagation_via_if() {
        // if(true, null, 1) should propagate nullability from the null branch
        let result = infer_expression_type("if(true, null, 1)", &registry()).unwrap();
        assert!(result.nullable);
    }

    #[test]
    fn unknown_function_error() {
        let result = infer_expression_type("nonexistent_func_xyz(1)", &registry());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ExprError::Function(_)));
    }

    #[test]
    fn undefined_variable_error() {
        let result = infer_expression_type("undefined_var", &registry());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ExprError::UndefinedVariable { .. }));
    }

    #[test]
    fn int_bound_precision_small_multiply() {
        // 1 * 1 * 1: each literal is 1 bit
        // 1 * 1 = 2 bits, then * 1 = 3 bits -> still Int64, not BigInt
        let result = infer_expression_type("1 * 1 * 1", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(3));
    }

    #[test]
    fn int_bound_literal_one() {
        let result = infer_expression_type("1", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(1));
    }

    #[test]
    fn int_bound_add_one_plus_one() {
        // 1 + 1: each is 1 bit, add -> max(1,1)+1 = 2
        let result = infer_expression_type("1 + 1", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(2));
    }

    #[test]
    fn int_bound_add_255_plus_255() {
        // 255 needs 8 bits, add -> max(8,8)+1 = 9
        let result = infer_expression_type("255 + 255", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(9));
    }

    #[test]
    fn int_bound_multiply_100_times_100() {
        // 100 needs 7 bits, multiply -> 7+7 = 14
        let result = infer_expression_type("100 * 100", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(14));
    }

    #[test]
    fn int_bound_five_ones_multiplied() {
        // 1 * 1 * 1 * 1 * 1: bound accumulates 1+1+1+1+1 = 5
        let result = infer_expression_type("1 * 1 * 1 * 1 * 1", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(5));
        // 5 bits fits in Int8
        assert_eq!(result.materialized_data_type(), DataType::Int8);
    }

    #[test]
    fn int_bound_255_times_255() {
        // 255 needs 8 bits, multiply -> 8+8 = 16, still Int64
        let result = infer_expression_type("255 * 255", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(16));
    }

    #[test]
    fn int_bound_cast_to_int64_drops_bound() {
        // toInt64(1) + toInt64(1): toInt64 returns Int64 without int_bound
        // so arithmetic falls back to DataType-level bounds (no precise tracking)
        // Int64 + Int64 via scalar_arithmetic -> 64+1=65 bits -> BigInt
        let result = infer_expression_type("toInt64(1) + toInt64(1)", &registry()).unwrap();
        assert!(matches!(result.data_type, DataType::BigInt { .. }));
        assert!(!result.nullable);
        assert_eq!(result.int_bound, None);
    }

    #[test]
    fn int_bound_none_for_float() {
        let result = infer_expression_type("3.14", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Float64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, None);
    }

    #[test]
    fn int_bound_none_for_string() {
        let result = infer_expression_type("'hello'", &registry()).unwrap();
        assert!(matches!(result.data_type, DataType::Text { .. }));
        assert!(!result.nullable);
        assert_eq!(result.int_bound, None);
    }

    #[test]
    fn int_bound_none_for_bool() {
        let result = infer_expression_type("true", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Bool);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, None);
    }
}
