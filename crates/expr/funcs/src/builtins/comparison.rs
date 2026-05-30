//! Comparison builtins (`==`, `!=`, `<`, `>`, `<=`, `>=`).
//!
//! All six are **total** — they never return null, so their result type is
//! non-null `Bool`. Equality treats null as an ordinary value: `null == null` is
//! `true`, `null == <non-null>` is `false` (matching `values_equal`/`Key`, the
//! canonical equality the type model already uses). The ordering operators treat
//! null as **incomparable**: any null operand yields `false` — null is not less,
//! greater, or order-equal to anything (mirrors SQL filtering, and deliberately
//! unlike `==`, which is the dedicated null test).
//!
//! For non-null operands the ordering operators delegate to the canonical
//! cross-numeric [`compare_values`], so mixed numeric widths (e.g. `BigInt` vs
//! `Int64`) compare correctly. An undefined comparison — `NaN`, or types with no
//! shared order — also yields `false` (matching IEEE `NaN` semantics;
//! cross-category pairs are additionally rejected at `resolve_type`).

use std::cmp::Ordering;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value, compare_values};

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

struct EqualsFunc;

impl ExprFunction for EqualsFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn can_fail(&self) -> bool {
        false
    }

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
        validate_comparable_args("equals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        // Total equality: `null == null` is true, `null == <non-null>` is false.
        let equal = match (a.is_null(), b.is_null()) {
            (true, true) => true,
            (true, false) | (false, true) => false,
            (false, false) => a == b,
        };
        Ok(Value::Bool(equal))
    }
}

struct NotEqualsFunc;

impl ExprFunction for NotEqualsFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn can_fail(&self) -> bool {
        false
    }

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
        validate_comparable_args("notEquals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        // Total inequality: the negation of total equality (`null != null` is false).
        let equal = match (a.is_null(), b.is_null()) {
            (true, true) => true,
            (true, false) | (false, true) => false,
            (false, false) => a == b,
        };
        Ok(Value::Bool(!equal))
    }
}

struct GreaterFunc;

impl ExprFunction for GreaterFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn can_fail(&self) -> bool {
        false
    }

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
        validate_comparable_args("greater", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Bool(false));
        }
        Ok(Value::Bool(
            compare_values(&a, &b) == Some(Ordering::Greater),
        ))
    }
}

struct LessFunc;

impl ExprFunction for LessFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn can_fail(&self) -> bool {
        false
    }

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
        validate_comparable_args("less", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Bool(false));
        }
        Ok(Value::Bool(compare_values(&a, &b) == Some(Ordering::Less)))
    }
}

struct GreaterOrEqualsFunc;

impl ExprFunction for GreaterOrEqualsFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn can_fail(&self) -> bool {
        false
    }

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
        validate_comparable_args("greaterOrEquals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Bool(false));
        }
        Ok(Value::Bool(matches!(
            compare_values(&a, &b),
            Some(Ordering::Greater | Ordering::Equal)
        )))
    }
}

struct LessOrEqualsFunc;

impl ExprFunction for LessOrEqualsFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn can_fail(&self) -> bool {
        false
    }

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
        validate_comparable_args("lessOrEquals", &args[0].data_type, &args[1].data_type)?;
        Ok(NullableExprType::non_null(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Bool(false));
        }
        Ok(Value::Bool(matches!(
            compare_values(&a, &b),
            Some(Ordering::Less | Ordering::Equal)
        )))
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
        // Bytes/Ipv4/Ipv6/Json/Xml share this catch-all: a same-"other" pair
        // passes resolve_type and resolves to `false` at runtime (compare_values
        // returns None); only cross-category pairs are rejected here.
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
    use super::*;
    use crate::test_support::ctx;

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
    fn equality_is_total_over_null() {
        // `==`/`!=` do not propagate null: `null == null` is true, `null ==
        // <non-null>` is false (the dedicated null test, matching `values_equal`).
        assert_eq!(
            EqualsFunc
                .evaluate(vec![Value::Null, Value::Int64(5)], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            EqualsFunc
                .evaluate(vec![Value::Null, Value::Null], &ctx())
                .unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            NotEqualsFunc
                .evaluate(vec![Value::Null, Value::Null], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            NotEqualsFunc
                .evaluate(vec![Value::Null, Value::Int64(5)], &ctx())
                .unwrap(),
            Value::Bool(true)
        );
        // Null on the RIGHT operand for equals (the other match arm).
        assert_eq!(
            EqualsFunc
                .evaluate(vec![Value::Int64(5), Value::Null], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
        // Cross-numeric equality of non-null values is by value, not variant.
        assert_eq!(
            EqualsFunc
                .evaluate(vec![Value::Int8(5), Value::Int64(5)], &ctx())
                .unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn resolve_type_is_non_null_bool_even_for_nullable_args() {
        // The headline of the totalisation: a comparison never returns null, so
        // its resolved type is non-null Bool regardless of operand nullability.
        let nullable = NullableExprType::new(DataType::Int64, true);
        let equality = EqualsFunc
            .resolve_type(&[nullable.clone(), nullable.clone()])
            .unwrap();
        assert_eq!(equality.data_type, DataType::Bool);
        assert!(!equality.nullable);
        let ordering = LessFunc
            .resolve_type(&[nullable.clone(), nullable])
            .unwrap();
        assert_eq!(ordering.data_type, DataType::Bool);
        assert!(!ordering.nullable);
    }

    #[test]
    fn ordering_with_null_is_false() {
        // Any null operand makes an ordering comparison false (null is unordered),
        // regardless of which side it is on.
        for other in [Value::Int64(5), Value::Null] {
            assert_eq!(
                LessFunc
                    .evaluate(vec![Value::Null, other.clone()], &ctx())
                    .unwrap(),
                Value::Bool(false)
            );
            assert_eq!(
                GreaterFunc
                    .evaluate(vec![other.clone(), Value::Null], &ctx())
                    .unwrap(),
                Value::Bool(false)
            );
        }
        assert_eq!(
            LessOrEqualsFunc
                .evaluate(vec![Value::Null, Value::Null], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            GreaterOrEqualsFunc
                .evaluate(vec![Value::Int64(5), Value::Null], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
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

    #[test]
    fn less_cross_numeric_int_widths() {
        let f = LessFunc;
        let result = f
            .evaluate(vec![Value::Int8(1), Value::Int64(2)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn less_bigint_promoted_against_int64() {
        // `(i64::MAX + 1)` promotes to BigInt; comparing it against an Int64
        // must work cross-numeric (a huge positive value is not < 0).
        let f = LessFunc;
        let big = Value::BigInt(num_bigint::BigInt::from(i64::MAX) + 1);
        let result = f.evaluate(vec![big, Value::Int64(0)], &ctx()).unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn greater_int_vs_float() {
        let f = GreaterFunc;
        let result = f
            .evaluate(vec![Value::Int64(3), Value::Float64(2.5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn nan_comparisons_are_false() {
        // NaN has no ordering: every comparison against it is false.
        let nan = Value::Float64(f64::NAN);
        assert_eq!(
            LessFunc
                .evaluate(vec![nan.clone(), Value::Float64(1.0)], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            LessOrEqualsFunc
                .evaluate(vec![nan.clone(), nan.clone()], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            GreaterOrEqualsFunc
                .evaluate(vec![nan.clone(), Value::Float64(1.0)], &ctx())
                .unwrap(),
            Value::Bool(false)
        );
    }
}
