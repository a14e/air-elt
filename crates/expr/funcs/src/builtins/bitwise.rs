use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

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

/// Rejects a shift amount outside `0..64`, matching the runtime check in both
/// shift functions' `evaluate`.
fn validate_shift_amount(func_name: &str, n: i64) -> Result<(), FuncError> {
    if !(0..64).contains(&n) {
        return Err(FuncError::InvalidArgument {
            function: func_name.to_owned(),
            message: format!("shift amount must be in 0..63, got {n}"),
        });
    }
    Ok(())
}

/// Validates a constant shift-amount argument (arg index 1) for the shift
/// functions. Dynamic or non-`Int64` amounts are skipped.
fn validate_shift_const(func_name: &str, args: &[Option<&Value>]) -> Result<(), FuncError> {
    if let Some(Some(Value::Int64(n))) = args.get(1) {
        validate_shift_amount(func_name, *n)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bitwise Operations
// ---------------------------------------------------------------------------

struct BitAndFunc;

impl ExprFunction for BitAndFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        let b_val = args.read(1);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitAnd")?;
        let b = to_i64(b_val, "bitAnd")?;
        Ok(Value::Int64(a & b))
    }
}

struct BitOrFunc;

impl ExprFunction for BitOrFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        let b_val = args.read(1);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitOr")?;
        let b = to_i64(b_val, "bitOr")?;
        Ok(Value::Int64(a | b))
    }
}

struct BitXorFunc;

impl ExprFunction for BitXorFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        let b_val = args.read(1);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitXor")?;
        let b = to_i64(b_val, "bitXor")?;
        Ok(Value::Int64(a ^ b))
    }
}

struct BitNotFunc;

impl ExprFunction for BitNotFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        if a_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitNot")?;
        Ok(Value::Int64(!a))
    }
}

struct BitShiftLeftFunc;

impl ExprFunction for BitShiftLeftFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        let n_val = args.read(1);
        if a_val.is_null() || n_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitShiftLeft")?;
        let n = to_i64(n_val, "bitShiftLeft")?;
        validate_shift_amount("bitShiftLeft", n)?;
        Ok(Value::Int64(a << n))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_shift_const("bitShiftLeft", args)
    }
}

struct BitShiftRightFunc;

impl ExprFunction for BitShiftRightFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        let n_val = args.read(1);
        if a_val.is_null() || n_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitShiftRight")?;
        let n = to_i64(n_val, "bitShiftRight")?;
        validate_shift_amount("bitShiftRight", n)?;
        // Arithmetic right shift (preserves sign) — Rust's default for i64
        Ok(Value::Int64(a >> n))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_shift_const("bitShiftRight", args)
    }
}

struct BitCountFunc;

impl ExprFunction for BitCountFunc {
    fn is_pure(&self) -> bool {
        true
    }

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

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a_val = args.read(0);
        if a_val.is_null() {
            return Ok(Value::Null);
        }
        let a = to_i64(a_val, "bitCount")?;
        Ok(Value::Int64(a.count_ones() as i64))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn bit_and_basic() {
        let result = eval(
            &BIT_AND,
            smallvec::smallvec![Value::Int64(0xFF), Value::Int64(0x0F)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(0x0F));
    }

    #[test]
    fn bit_or_basic() {
        let result = eval(
            &BIT_OR,
            smallvec::smallvec![Value::Int64(0xF0), Value::Int64(0x0F)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(0xFF));
    }

    #[test]
    fn bit_xor_basic() {
        let result = eval(
            &BIT_XOR,
            smallvec::smallvec![Value::Int64(0xFF), Value::Int64(0x0F)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(0xF0));
    }

    #[test]
    fn bit_not_zero() {
        // !0 in two's complement = -1 (all bits set)
        let result = eval(&BIT_NOT, smallvec::smallvec![Value::Int64(0)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(-1));
    }

    #[test]
    fn bit_shift_left_basic() {
        let result = eval(
            &BIT_SHIFT_LEFT,
            smallvec::smallvec![Value::Int64(1), Value::Int64(8)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(256));
    }

    #[test]
    fn bit_shift_right_basic() {
        let result = eval(
            &BIT_SHIFT_RIGHT,
            smallvec::smallvec![Value::Int64(256), Value::Int64(8)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn bit_shift_left_out_of_range() {
        let result = eval(
            &BIT_SHIFT_LEFT,
            smallvec::smallvec![Value::Int64(1), Value::Int64(64)],
            &ctx(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn bit_shift_right_out_of_range() {
        let result = eval(
            &BIT_SHIFT_RIGHT,
            smallvec::smallvec![Value::Int64(1), Value::Int64(-1)],
            &ctx(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn bit_count_basic() {
        let result = eval(&BIT_COUNT, smallvec::smallvec![Value::Int64(0xFF)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(8));
    }

    #[test]
    fn bit_and_null_propagation() {
        let result = eval(
            &BIT_AND,
            smallvec::smallvec![Value::Null, Value::Int64(0xFF)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn bit_not_null_propagation() {
        let result = eval(&BIT_NOT, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn bit_count_null_propagation() {
        let result = eval(&BIT_COUNT, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn validate_const_shift_valid_ok() {
        let n = Value::Int64(8);
        let result = BIT_SHIFT_LEFT.validate_const_args(&[None, Some(&n)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_shift_boundaries_ok() {
        // 0 and 63 are the inclusive in-range edges; 64 (tested separately) is out.
        for amount in [0_i64, 63] {
            let n = Value::Int64(amount);
            let left = BIT_SHIFT_LEFT.validate_const_args(&[None, Some(&n)], &ctx());
            assert!(left.is_ok(), "bitShiftLeft must accept shift {amount}");
            let right = BIT_SHIFT_RIGHT.validate_const_args(&[None, Some(&n)], &ctx());
            assert!(right.is_ok(), "bitShiftRight must accept shift {amount}");
        }
    }

    #[test]
    fn validate_const_shift_dynamic_ok() {
        let result = BIT_SHIFT_RIGHT.validate_const_args(&[None, None], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_shift_out_of_range_errors() {
        let n = Value::Int64(64);
        let result = BIT_SHIFT_LEFT.validate_const_args(&[None, Some(&n)], &ctx());
        assert!(matches!(result, Err(FuncError::InvalidArgument { .. })));

        let neg = Value::Int64(-1);
        let result = BIT_SHIFT_RIGHT.validate_const_args(&[None, Some(&neg)], &ctx());
        assert!(matches!(result, Err(FuncError::InvalidArgument { .. })));
    }
}
