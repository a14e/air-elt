use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static IF: IfFunc = IfFunc;
static MULTI_IF: MultiIfFunc = MultiIfFunc;
static IF_NULL: IfNullFunc = IfNullFunc;
static NULL_IF: NullIfFunc = NullIfFunc;
static COALESCE: CoalesceFunc = CoalesceFunc;
static IS_NULL: IsNullFunc = IsNullFunc;
static IS_NOT_NULL: IsNotNullFunc = IsNotNullFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&IF);
    registry.register(&MULTI_IF);
    registry.register(&IF_NULL);
    registry.register(&NULL_IF);
    registry.register(&COALESCE);
    registry.register(&IS_NULL);
    registry.register(&IS_NOT_NULL);
}

struct IfFunc;

impl ExprFunction for IfFunc {
    fn name(&self) -> &str {
        "if"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args[1].nullable || args[2].nullable || args[0].nullable;
        Ok(NullableExprType::new(args[1].data_type.clone(), nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let else_val = args.remove(2);
        let then_val = args.remove(1);
        let cond = args.remove(0);
        match cond {
            Value::Bool(true) => Ok(then_val),
            Value::Bool(false) | Value::Null => Ok(else_val),
            other => Err(FuncError::TypeMismatch {
                function: "if".to_owned(),
                expected: "Bool".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct MultiIfFunc;

impl ExprFunction for MultiIfFunc {
    fn name(&self) -> &str {
        "multiIf"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        if args.len().is_multiple_of(2) {
            return Err(FuncError::ArityMismatch {
                function: "multiIf".to_owned(),
                expected: "odd number of arguments".to_owned(),
                actual: args.len(),
            });
        }
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(args[1].data_type.clone(), nullable))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        if args.len().is_multiple_of(2) {
            return Err(FuncError::ArityMismatch {
                function: "multiIf".to_owned(),
                expected: "odd number of arguments".to_owned(),
                actual: args.len(),
            });
        }
        let pairs = args.len() / 2;
        for i in 0..pairs {
            let cond = &args[i * 2];
            match cond {
                Value::Bool(true) => return Ok(args.into_iter().nth(i * 2 + 1).expect("checked")),
                Value::Bool(false) | Value::Null => continue,
                other => {
                    return Err(FuncError::TypeMismatch {
                        function: "multiIf".to_owned(),
                        expected: "Bool".to_owned(),
                        actual: format!("{:?}", other.data_type()),
                    });
                }
            }
        }
        Ok(args.into_iter().last().expect("checked"))
    }
}

struct IfNullFunc;

impl ExprFunction for IfNullFunc {
    fn name(&self) -> &str {
        "ifNull"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args[1].nullable;
        Ok(NullableExprType::new(args[0].data_type.clone(), nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let alt = args.remove(1);
        let val = args.remove(0);
        if val.is_null() { Ok(alt) } else { Ok(val) }
    }
}

struct NullIfFunc;

impl ExprFunction for NullIfFunc {
    fn name(&self) -> &str {
        "nullIf"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(args[0].data_type.clone()))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let sentinel = args.remove(1);
        let val = args.remove(0);
        if val == sentinel {
            Ok(Value::Null)
        } else {
            Ok(val)
        }
    }
}

struct CoalesceFunc;

impl ExprFunction for CoalesceFunc {
    fn name(&self) -> &str {
        "coalesce"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let all_nullable = args.iter().all(|a| a.nullable);
        Ok(NullableExprType::new(
            args[0].data_type.clone(),
            all_nullable,
        ))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        for val in args {
            if !val.is_null() {
                return Ok(val);
            }
        }
        Ok(Value::Null)
    }
}

struct IsNullFunc;

impl ExprFunction for IsNullFunc {
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        Ok(Value::Bool(a.is_null()))
    }
}

struct IsNotNullFunc;

impl ExprFunction for IsNotNullFunc {
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        Ok(Value::Bool(!a.is_null()))
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
    fn if_true_branch() {
        let f = IfFunc;
        let result = f
            .evaluate(
                vec![Value::Bool(true), Value::Int64(1), Value::Int64(2)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn if_false_branch() {
        let f = IfFunc;
        let result = f
            .evaluate(
                vec![Value::Bool(false), Value::Int64(1), Value::Int64(2)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn if_null_condition_returns_else() {
        let f = IfFunc;
        let result = f
            .evaluate(vec![Value::Null, Value::Int64(1), Value::Int64(2)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn multi_if_second_condition() {
        let f = MultiIfFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Bool(false),
                    Value::Int64(1),
                    Value::Bool(true),
                    Value::Int64(2),
                    Value::Int64(3),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn multi_if_else_branch() {
        let f = MultiIfFunc;
        let result = f
            .evaluate(
                vec![
                    Value::Bool(false),
                    Value::Int64(1),
                    Value::Bool(false),
                    Value::Int64(2),
                    Value::Int64(99),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(99));
    }

    #[test]
    fn if_null_replaces() {
        let f = IfNullFunc;
        let result = f
            .evaluate(vec![Value::Null, Value::Int64(42)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn if_null_keeps_non_null() {
        let f = IfNullFunc;
        let result = f
            .evaluate(vec![Value::Int64(7), Value::Int64(42)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(7));
    }

    #[test]
    fn null_if_matches() {
        let f = NullIfFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_if_no_match() {
        let f = NullIfFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn coalesce_picks_first_non_null() {
        let f = CoalesceFunc;
        let result = f
            .evaluate(
                vec![Value::Null, Value::Null, Value::Int64(3), Value::Int64(4)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn coalesce_all_null() {
        let f = CoalesceFunc;
        let result = f.evaluate(vec![Value::Null, Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn is_null_true() {
        let f = IsNullFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn is_null_false() {
        let f = IsNullFunc;
        let result = f.evaluate(vec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn is_not_null() {
        let f = IsNotNullFunc;
        let result = f.evaluate(vec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(true));
    }
}
