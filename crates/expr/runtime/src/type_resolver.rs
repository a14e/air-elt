use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_parse::model::{
    ConditionalExpr, Expr, InterpolationSegment, LiteralValue, Program, Statement,
};
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::DataType;

use crate::error::ExprError;

pub struct TypeResolver<'a> {
    registry: &'a FunctionRegistry,
}

impl<'a> TypeResolver<'a> {
    pub fn create(registry: &'a FunctionRegistry) -> Self {
        Self { registry }
    }

    pub fn infer_type(&self, program: &Program) -> Result<NullableExprType, ExprError> {
        let mut checker = TypeCheckerState::new(self.registry);
        checker.check_program(program)
    }
}

struct TypeCheckerState<'a> {
    registry: &'a FunctionRegistry,
    variables: AHashMap<String, NullableExprType>,
}

impl<'a> TypeCheckerState<'a> {
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

    fn check_expr(&mut self, expr: &Expr) -> Result<NullableExprType, ExprError> {
        match expr {
            Expr::Literal(literal) => Ok(self.check_literal(literal)),
            Expr::Variable(name) => self.check_variable(name),
            Expr::FunctionCall { name, args } => self.check_function_call(name, args),
            Expr::Conditional(conditional) => self.check_conditional(conditional),
            Expr::Interpolation(segments) => self.check_interpolation(segments),
            Expr::Object(entries) => self.check_object(entries),
            Expr::Block { statements, result } => self.check_block(statements, result),
            Expr::Field(..) | Expr::Fields(..) => Err(ExprError::FieldOutsideTransform),
        }
    }

    /// Type-check a scoped-binding block: each binding's type shadows its name
    /// in the type environment for the remainder of the block; the block's type
    /// is the result type under that extended environment. After the result is
    /// checked the displaced entries are restored in reverse insertion order
    /// (on both success and error paths), so the outer scope and sibling
    /// branches see the pre-block types again — mirroring the evaluator's
    /// runtime scoping exactly.
    fn check_block(
        &mut self,
        statements: &[Statement],
        result: &Expr,
    ) -> Result<NullableExprType, ExprError> {
        let mut displaced: Vec<(&str, Option<NullableExprType>)> =
            Vec::with_capacity(statements.len());
        let outcome = self.check_block_scope(statements, result, &mut displaced);

        // Entries borrow the AST names; only a shadowed restore re-owns one.
        for (name, previous) in displaced.into_iter().rev() {
            match previous {
                Some(inferred_type) => {
                    self.variables.insert(name.to_owned(), inferred_type);
                }
                None => {
                    self.variables.remove(name);
                }
            }
        }

        outcome
    }

    fn check_block_scope<'s>(
        &mut self,
        statements: &'s [Statement],
        result: &Expr,
        displaced: &mut Vec<(&'s str, Option<NullableExprType>)>,
    ) -> Result<NullableExprType, ExprError> {
        for statement in statements {
            let inferred_type = self.check_expr(&statement.value)?;
            let previous = self.variables.insert(statement.name.clone(), inferred_type);
            displaced.push((&statement.name, previous));
        }
        self.check_expr(result)
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
            LiteralValue::Interval(_) => NullableExprType::non_null(DataType::Interval),
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
        &mut self,
        name: &str,
        args: &[Expr],
    ) -> Result<NullableExprType, ExprError> {
        let arg_types: Vec<NullableExprType> = args
            .iter()
            .map(|arg| self.check_expr(arg))
            .collect::<Result<Vec<_>, _>>()?;

        let func_ref = self.registry.get_ref(name, Some(args.len()))?;
        let function = self.registry.get_by_ref(func_ref);
        let result_type = function.resolve_type(&arg_types)?;
        Ok(result_type)
    }

    fn check_conditional(
        &mut self,
        conditional: &ConditionalExpr,
    ) -> Result<NullableExprType, ExprError> {
        match conditional {
            ConditionalExpr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let _cond_type = self.check_expr(condition)?;
                let then_type = self.check_expr(then_branch)?;
                let else_type = self.check_expr(else_branch)?;
                let nullable = then_type.nullable || else_type.nullable;
                Ok(NullableExprType::new(then_type.data_type, nullable))
            }
            ConditionalExpr::MultiIf { branches, default } => {
                for (condition, _value) in branches {
                    let _cond_type = self.check_expr(condition)?;
                }
                let mut nullable = false;
                let mut data_type = None;
                for (_condition, value) in branches {
                    let val_type = self.check_expr(value)?;
                    if data_type.is_none() {
                        data_type = Some(val_type.data_type.clone());
                    }
                    nullable = nullable || val_type.nullable;
                }
                let default_type = self.check_expr(default)?;
                nullable = nullable || default_type.nullable;
                let result_data_type = data_type.unwrap_or(default_type.data_type);
                Ok(NullableExprType::new(result_data_type, nullable))
            }
            ConditionalExpr::IfNull { value, alternative } => {
                let value_type = self.check_expr(value)?;
                let alt_type = self.check_expr(alternative)?;
                Ok(NullableExprType::new(
                    value_type.data_type,
                    alt_type.nullable,
                ))
            }
            ConditionalExpr::NullIf { value, sentinel } => {
                let value_type = self.check_expr(value)?;
                let _sentinel_type = self.check_expr(sentinel)?;
                // Always nullable since it can return null.
                Ok(NullableExprType::nullable(value_type.data_type))
            }
            ConditionalExpr::And { left, right } => {
                let left_type = self.check_expr(left)?;
                let right_type = self.check_expr(right)?;
                let nullable = left_type.nullable || right_type.nullable;
                Ok(NullableExprType::new(DataType::Bool, nullable))
            }
            ConditionalExpr::Or { left, right } => {
                let left_type = self.check_expr(left)?;
                let right_type = self.check_expr(right)?;
                let nullable = left_type.nullable || right_type.nullable;
                Ok(NullableExprType::new(DataType::Bool, nullable))
            }
        }
    }

    fn check_interpolation(
        &mut self,
        segments: &[InterpolationSegment],
    ) -> Result<NullableExprType, ExprError> {
        for segment in segments {
            if let InterpolationSegment::Expression(expr) = segment {
                self.check_expr(expr)?;
            }
        }
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn check_object(&mut self, entries: &[(String, Expr)]) -> Result<NullableExprType, ExprError> {
        for (_key, value_expr) in entries {
            self.check_expr(value_expr)?;
        }
        Ok(NullableExprType::non_null(DataType::Object))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_expr_funcs::FunctionRegistry;
    use air_elt_expr_parse::Parser;

    fn registry() -> FunctionRegistry {
        FunctionRegistry::with_builtins()
    }

    fn infer_expression_type(
        input: &str,
        registry: &FunctionRegistry,
    ) -> Result<NullableExprType, ExprError> {
        let program = Parser::create().parse_expression(input)?;
        let resolver = TypeResolver::create(registry);
        resolver.infer_type(&program)
    }

    #[test]
    fn literal_int_type() {
        let result = infer_expression_type("42", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
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
        assert_eq!(result.int_bound, Some(6));
    }

    #[test]
    fn function_return_type() {
        let result = infer_expression_type("if(true, 1, 2)", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, None);
    }

    #[test]
    fn interpolation_type() {
        let result = infer_expression_type("'hello ${1 + 2} world'", &registry());
        assert!(result.is_ok());
        let typ = result.unwrap();
        assert!(!typ.nullable);
        assert!(matches!(typ.data_type, DataType::Text { .. }));
    }

    #[test]
    fn object_type() {
        let result = infer_expression_type("object('key', 1)", &registry());
        if let Ok(typ) = result {
            assert!(!typ.nullable);
        }
    }

    #[test]
    fn interpolation_catches_undefined_variable() {
        let result = infer_expression_type("\"hello {undefined_var} world\"", &registry());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ExprError::UndefinedVariable { .. }));
    }

    #[test]
    fn object_catches_undefined_variable() {
        let result = infer_expression_type("{\"key\" = undefined_var}", &registry());
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ExprError::UndefinedVariable { .. }));
    }

    #[test]
    fn concat_returns_text() {
        let result = infer_expression_type("concat('abc', 'def')", &registry()).unwrap();
        assert_eq!(
            result,
            NullableExprType::non_null(DataType::Text { size: Some(6) })
        );
    }

    #[test]
    fn add_text_concat_bounded() {
        let result = infer_expression_type("'abc' + 'def'", &registry()).unwrap();
        assert!(!result.nullable);
        assert!(matches!(result.data_type, DataType::Text { .. }));
    }

    #[test]
    fn null_propagation_via_if() {
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
        let result = infer_expression_type("1 + 1", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(2));
    }

    #[test]
    fn int_bound_add_255_plus_255() {
        let result = infer_expression_type("255 + 255", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(9));
    }

    #[test]
    fn int_bound_multiply_100_times_100() {
        let result = infer_expression_type("100 * 100", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(14));
    }

    #[test]
    fn int_bound_five_ones_multiplied() {
        let result = infer_expression_type("1 * 1 * 1 * 1 * 1", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(5));
        assert_eq!(result.materialized_data_type(), DataType::Int8);
    }

    #[test]
    fn int_bound_255_times_255() {
        let result = infer_expression_type("255 * 255", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
        assert_eq!(result.int_bound, Some(16));
    }

    #[test]
    fn int_bound_cast_to_int64_drops_bound() {
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

    #[test]
    fn block_type_is_result_type() {
        let result =
            infer_expression_type("if (true) { x = 2; x + 1 } else 0", &registry()).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert!(!result.nullable);
    }

    #[test]
    fn block_scoped_type_shadowing() {
        // Outer `x` is Int; inside the block `x` is shadowed by Text. The block
        // result `x` then resolves to Text; after the block the outer Int `x`
        // is restored and concatenated (Int + Int) keeps the whole program
        // type-checking.
        let source = "x = 1; y = if (true) { x = 'hi'; length(x) } else 0; x + y";
        let result = infer_expression_type(source, &registry()).unwrap();
        // The block accepted Text for the shadowed `x` (`length` is Text-strict,
        // so it would error if the outer Int leaked in); after the block the
        // outer Int `x` is restored, so `x + y` type-checks as an integer.
        // Exact type: `If` resolution drops `int_bound` (`NullableExprType::new`
        // sets it to None), so the addition falls back to DataType-level bits
        // (64 + 1) and promotes to BigInt.
        assert!(matches!(result.data_type, DataType::BigInt { .. }));
        assert!(!result.nullable);
    }

    #[test]
    fn block_local_type_undefined_after_if() {
        let source = "y = if (true) { t = 5; t } else 0; t + y";
        let result = infer_expression_type(source, &registry());
        let err = result.unwrap_err();
        assert!(matches!(err, ExprError::UndefinedVariable { name } if name == "t"));
    }

    #[test]
    fn block_nullability_through_branches() {
        // One branch yields a nullable value (null literal), so the if-type is
        // nullable even though the other branch's block result is non-null.
        let source = "if (true) { x = 1; x } else { y = null; y }";
        let result = infer_expression_type(source, &registry()).unwrap();
        assert!(result.nullable);
    }

    #[test]
    fn block_non_null_branches_stay_non_null() {
        let source = "if (true) { x = 1; x } else { y = 2; y }";
        let result = infer_expression_type(source, &registry()).unwrap();
        assert!(!result.nullable);
        assert_eq!(result.data_type, DataType::Int64);
    }
}
