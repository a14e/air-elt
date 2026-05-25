use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static BIT_AND: BitAndFunc = BitAndFunc;
static BIT_OR: BitOrFunc = BitOrFunc;
static BIT_XOR: BitXorFunc = BitXorFunc;
static BIT_NOT: BitNotFunc = BitNotFunc;
static BIT_SHIFT_LEFT: BitShiftLeftFunc = BitShiftLeftFunc;
static BIT_SHIFT_RIGHT: BitShiftRightFunc = BitShiftRightFunc;
static BIT_COUNT: BitCountFunc = BitCountFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&BIT_AND);
    registry.register(&BIT_OR);
    registry.register(&BIT_XOR);
    registry.register(&BIT_NOT);
    registry.register(&BIT_SHIFT_LEFT);
    registry.register(&BIT_SHIFT_RIGHT);
    registry.register(&BIT_COUNT);
}

fn to_i64(val: &Value, func_name: &str) -> Result<i64, FuncError> {
    match val {
        Value::Int64(x) => Ok(*x),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Int64".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

// ---------------------------------------------------------------------------
// Bitwise Operations
// ---------------------------------------------------------------------------

struct BitAndFunc;

impl ExprFunction for BitAndFunc {
    fn name(&self) -> &str {
        "bitAnd"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b_val = args.remove(1);
        let a_val = args.remove(0);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitAnd")?;
        let b = to_i64(&b_val, "bitAnd")?;
        Ok(Value::Int64(a & b))
    }
}

struct BitOrFunc;

impl ExprFunction for BitOrFunc {
    fn name(&self) -> &str {
        "bitOr"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b_val = args.remove(1);
        let a_val = args.remove(0);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitOr")?;
        let b = to_i64(&b_val, "bitOr")?;
        Ok(Value::Int64(a | b))
    }
}

struct BitXorFunc;

impl ExprFunction for BitXorFunc {
    fn name(&self) -> &str {
        "bitXor"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b_val = args.remove(1);
        let a_val = args.remove(0);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitXor")?;
        let b = to_i64(&b_val, "bitXor")?;
        Ok(Value::Int64(a ^ b))
    }
}

struct BitNotFunc;

impl ExprFunction for BitNotFunc {
    fn name(&self) -> &str {
        "bitNot"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a_val = args.remove(0);
        if a_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitNot")?;
        Ok(Value::Int64(!a))
    }
}

struct BitShiftLeftFunc;

impl ExprFunction for BitShiftLeftFunc {
    fn name(&self) -> &str {
        "bitShiftLeft"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let n_val = args.remove(1);
        let a_val = args.remove(0);
        if a_val.is_null() || n_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitShiftLeft")?;
        let n = to_i64(&n_val, "bitShiftLeft")?;
        if !(0..64).contains(&n) {
            return Err(FuncError::InvalidArgument {
                function: "bitShiftLeft".to_owned(),
                message: format!("shift amount must be in 0..63, got {n}"),
            });
        }
        Ok(Value::Int64(a << n))
    }
}

struct BitShiftRightFunc;

impl ExprFunction for BitShiftRightFunc {
    fn name(&self) -> &str {
        "bitShiftRight"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let n_val = args.remove(1);
        let a_val = args.remove(0);
        if a_val.is_null() || n_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitShiftRight")?;
        let n = to_i64(&n_val, "bitShiftRight")?;
        if !(0..64).contains(&n) {
            return Err(FuncError::InvalidArgument {
                function: "bitShiftRight".to_owned(),
                message: format!("shift amount must be in 0..63, got {n}"),
            });
        }
        // Arithmetic right shift (preserves sign) — Rust's default for i64
        Ok(Value::Int64(a >> n))
    }
}

struct BitCountFunc;

impl ExprFunction for BitCountFunc {
    fn name(&self) -> &str {
        "bitCount"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a_val = args.remove(0);
        if a_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(&a_val, "bitCount")?;
        Ok(Value::Int64(a.count_ones() as i64))
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
            env_resolver: Arc::new(crate::test_support::EmptyEnv),
            file_resolver: Arc::new(crate::test_support::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn bit_and_basic() {
        let result = BIT_AND
            .evaluate(vec![Value::Int64(0xFF), Value::Int64(0x0F)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(0x0F));
    }

    #[test]
    fn bit_or_basic() {
        let result = BIT_OR
            .evaluate(vec![Value::Int64(0xF0), Value::Int64(0x0F)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(0xFF));
    }

    #[test]
    fn bit_xor_basic() {
        let result = BIT_XOR
            .evaluate(vec![Value::Int64(0xFF), Value::Int64(0x0F)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(0xF0));
    }

    #[test]
    fn bit_not_zero() {
        // !0 in two's complement = -1 (all bits set)
        let result = BIT_NOT.evaluate(vec![Value::Int64(0)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(-1));
    }

    #[test]
    fn bit_shift_left_basic() {
        let result = BIT_SHIFT_LEFT
            .evaluate(vec![Value::Int64(1), Value::Int64(8)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(256));
    }

    #[test]
    fn bit_shift_right_basic() {
        let result = BIT_SHIFT_RIGHT
            .evaluate(vec![Value::Int64(256), Value::Int64(8)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn bit_shift_left_out_of_range() {
        let result = BIT_SHIFT_LEFT.evaluate(vec![Value::Int64(1), Value::Int64(64)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn bit_shift_right_out_of_range() {
        let result = BIT_SHIFT_RIGHT.evaluate(vec![Value::Int64(1), Value::Int64(-1)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn bit_count_basic() {
        let result = BIT_COUNT
            .evaluate(vec![Value::Int64(0xFF)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(8));
    }

    #[test]
    fn bit_and_null_propagation() {
        let result = BIT_AND
            .evaluate(vec![Value::Null, Value::Int64(0xFF)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn bit_not_null_propagation() {
        let result = BIT_NOT.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn bit_count_null_propagation() {
        let result = BIT_COUNT.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }
}
