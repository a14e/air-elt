use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static EQUALS: EqualsFunc = EqualsFunc;
static NOT_EQUALS: NotEqualsFunc = NotEqualsFunc;
static GREATER: GreaterFunc = GreaterFunc;
static LESS: LessFunc = LessFunc;
static GREATER_OR_EQUALS: GreaterOrEqualsFunc = GreaterOrEqualsFunc;
static LESS_OR_EQUALS: LessOrEqualsFunc = LessOrEqualsFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&EQUALS);
    registry.register(&NOT_EQUALS);
    registry.register(&GREATER);
    registry.register(&LESS);
    registry.register(&GREATER_OR_EQUALS);
    registry.register(&LESS_OR_EQUALS);
}

fn compare_values(a: &Value, b: &Value) -> Result<std::cmp::Ordering, FuncError> {
    match (a, b) {
        (Value::Int64(x), Value::Int64(y)) => Ok(x.cmp(y)),
        (Value::Float64(x), Value::Float64(y)) => {
            Ok(x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal))
        }
        (Value::Text(x), Value::Text(y)) => Ok(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        _ => Err(FuncError::TypeMismatch {
            function: "comparison".to_owned(),
            expected: "matching comparable types".to_owned(),
            actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
        }),
    }
}

struct EqualsFunc;

impl ExprFunction for EqualsFunc {
    fn name(&self) -> &str {
        "equals"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_comparable_args("equals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        Ok(Value::Bool(a == b))
    }
}

struct NotEqualsFunc;

impl ExprFunction for NotEqualsFunc {
    fn name(&self) -> &str {
        "notEquals"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_comparable_args("notEquals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        Ok(Value::Bool(a != b))
    }
}

struct GreaterFunc;

impl ExprFunction for GreaterFunc {
    fn name(&self) -> &str {
        "greater"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_comparable_args("greater", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let ord = compare_values(&a, &b)?;
        Ok(Value::Bool(ord == std::cmp::Ordering::Greater))
    }
}

struct LessFunc;

impl ExprFunction for LessFunc {
    fn name(&self) -> &str {
        "less"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_comparable_args("less", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let ord = compare_values(&a, &b)?;
        Ok(Value::Bool(ord == std::cmp::Ordering::Less))
    }
}

struct GreaterOrEqualsFunc;

impl ExprFunction for GreaterOrEqualsFunc {
    fn name(&self) -> &str {
        "greaterOrEquals"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_comparable_args("greaterOrEquals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let ord = compare_values(&a, &b)?;
        Ok(Value::Bool(ord != std::cmp::Ordering::Less))
    }
}

struct LessOrEqualsFunc;

impl ExprFunction for LessOrEqualsFunc {
    fn name(&self) -> &str {
        "lessOrEquals"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_comparable_args("lessOrEquals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        let ord = compare_values(&a, &b)?;
        Ok(Value::Bool(ord != std::cmp::Ordering::Greater))
    }
}

fn type_category(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float32
        | DataType::Float64
        | DataType::BigInt { .. }
        | DataType::Decimal { .. } => "numeric",
        DataType::Text { .. } => "text",
        DataType::Bool => "bool",
        DataType::Date => "date",
        DataType::Timestamp => "timestamp",
        DataType::Uuid => "uuid",
        DataType::Object => "object",
        _ => "other",
    }
}

fn validate_comparable_args(
    function: &str,
    left: &DataType,
    right: &DataType,
) -> Result<(), FuncError> {
    let left_cat = type_category(left);
    let right_cat = type_category(right);
    if left_cat != right_cat {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: format!("matching comparable types (got {left_cat} and {right_cat})"),
            actual: format!("{left}, {right}"),
        });
    }
    Ok(())
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
            env_resolver: Arc::new(crate::test_support::EmptyEnv),
            file_resolver: Arc::new(crate::test_support::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn equals_same() {
        let f = EqualsFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn equals_different() {
        let f = EqualsFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn null_propagation() {
        let f = EqualsFunc;
        let result = f
            .evaluate(vec![Value::Null, Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn greater_int() {
        let f = GreaterFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn less_float() {
        let f = LessFunc;
        let result = f
            .evaluate(vec![Value::Float64(1.0), Value::Float64(2.0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn greater_or_equals_equal() {
        let f = GreaterOrEqualsFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn less_or_equals_text() {
        let f = LessOrEqualsFunc;
        let result = f
            .evaluate(
                vec![Value::Text("abc".into()), Value::Text("abd".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }
}
