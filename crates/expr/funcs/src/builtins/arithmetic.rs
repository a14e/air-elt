use std::cmp::Ordering;

use crate::arithmetic_utils::{
    ArithmeticOp, arithmetic_result_type, comparable_join, concat_result_type,
};
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value, compare_values};
use bigdecimal::{BigDecimal, RoundingMode};
use num_bigint::BigInt;
use num_traits::{ToPrimitive, Zero};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction, FuncArgVec};

/// Widen a narrow numeric `Value` to the canonical wide form the binary
/// arithmetic ops compute on (`Int64` / `Float64` / `BigInt` / `Decimal`).
/// Source rows carry narrow variants (`Int32` for a PG `INT`, `Float32`
/// for `REAL`, …); the type algebra already promotes to the wide form, so
/// the runtime must canonicalise the actual values to match. Non-numeric
/// values pass through unchanged.
fn widen_numeric(value: Value) -> Value {
    match value {
        Value::Int8(x) => Value::Int64(i64::from(x)),
        Value::Int16(x) => Value::Int64(i64::from(x)),
        Value::Int32(x) => Value::Int64(i64::from(x)),
        Value::UInt8(x) => Value::Int64(i64::from(x)),
        Value::UInt16(x) => Value::Int64(i64::from(x)),
        Value::UInt32(x) => Value::Int64(i64::from(x)),
        Value::UInt64(x) => match i64::try_from(x) {
            Ok(v) => Value::Int64(v),
            Err(_) => Value::BigInt(BigInt::from(x)),
        },
        Value::Float32(x) => Value::Float64(f64::from(x)),
        other => other,
    }
}

static ADD: AddFunc = AddFunc;
static SUBTRACT: SubtractFunc = SubtractFunc;
static MULTIPLY: MultiplyFunc = MultiplyFunc;
static DIVIDE: DivideFunc = DivideFunc;
static MODULO: ModuloFunc = ModuloFunc;
static NEGATE: NegateFunc = NegateFunc;
static ABS: AbsFunc = AbsFunc;
static CEIL: CeilFunc = CeilFunc;
static FLOOR: FloorFunc = FloorFunc;
static ROUND: RoundFunc = RoundFunc;
static MIN: MinFunc = MinFunc;
static MAX: MaxFunc = MaxFunc;
static SIGN: SignFunc = SignFunc;
static POWER: PowerFunc = PowerFunc;
static SQRT: SqrtFunc = SqrtFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&ADD);
    registry.register(&SUBTRACT);
    registry.register(&MULTIPLY);
    registry.register(&DIVIDE);
    registry.register(&MODULO);
    registry.register(&NEGATE);
    registry.register(&ABS);
    registry.register(&CEIL);
    registry.register(&FLOOR);
    registry.register(&ROUND);
    registry.register(&MIN);
    registry.register(&MAX);
    registry.register(&SIGN);
    registry.register(&POWER);
    registry.register(&SQRT);
}

struct AddFunc;

impl ExprFunction for AddFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "add"
    }

    fn can_fail(&self) -> bool {
        false
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        let both_text = matches!(
            (&args[0].data_type, &args[1].data_type),
            (DataType::Text { .. }, DataType::Text { .. })
        );
        if both_text {
            let result_dt =
                concat_result_type(&[&args[0].data_type, &args[1].data_type]).map_err(|e| {
                    FuncError::TypeMismatch {
                        function: "add".to_owned(),
                        expected: "Text".to_owned(),
                        actual: e.to_string(),
                    }
                })?;
            Ok(NullableExprType::new(result_dt, nullable))
        } else {
            validate_numeric_args("add", &args[0].data_type, &args[1].data_type)?;
            let result =
                arithmetic_result_type(ArithmeticOp::Add, &args[0], &args[1]).map_err(|e| {
                    FuncError::TypeMismatch {
                        function: "add".to_owned(),
                        expected: "numeric".to_owned(),
                        actual: e.to_string(),
                    }
                })?;
            Ok(NullableExprType { nullable, ..result })
        }
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        // Take ownership of arg 0 (its buffer is reused for Text concat /
        // BigInt / Decimal). For the Text case arg 1 is only READ —
        // `push_str` borrows it, so taking would clone a const separator
        // (`' '`, `'-'`) on every row.
        let left = args.take(0);
        if let Value::Text(mut s) = left {
            return match args.read(1) {
                Value::Text(t) => {
                    s.push_str(t);
                    Ok(Value::Text(s))
                }
                other => Err(FuncError::TypeMismatch {
                    function: "add".to_owned(),
                    expected: "matching numeric or text types".to_owned(),
                    actual: format!("Text, {:?}", other.data_type()),
                }),
            };
        }
        // Numeric: canonicalise narrow operands (`Int32`, `Float32`, …) to the
        // wide form the arms compute on.
        match (widen_numeric(left), widen_numeric(args.take(1))) {
            (Value::Int64(x), Value::Int64(y)) => match x.checked_add(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) + BigInt::from(y))),
            },
            (Value::Float64(x), Value::Float64(y)) => Ok(Value::Float64(x + y)),
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x + y)),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x + BigInt::from(y))),
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) + y)),
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x + y)),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x + BigDecimal::from(y))),
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) + y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "add".to_owned(),
                expected: "matching numeric or text types".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct SubtractFunc;

impl ExprFunction for SubtractFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "subtract"
    }

    fn can_fail(&self) -> bool {
        false
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_numeric_args("subtract", &args[0].data_type, &args[1].data_type)?;
        let result =
            arithmetic_result_type(ArithmeticOp::Subtract, &args[0], &args[1]).map_err(|e| {
                FuncError::TypeMismatch {
                    function: "subtract".to_owned(),
                    expected: "numeric".to_owned(),
                    actual: e.to_string(),
                }
            })?;
        Ok(NullableExprType { nullable, ..result })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        match (widen_numeric(args.take(0)), widen_numeric(args.take(1))) {
            (Value::Int64(x), Value::Int64(y)) => match x.checked_sub(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) - BigInt::from(y))),
            },
            (Value::Float64(x), Value::Float64(y)) => Ok(Value::Float64(x - y)),
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x - y)),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x - BigInt::from(y))),
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) - y)),
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x - y)),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x - BigDecimal::from(y))),
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) - y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "subtract".to_owned(),
                expected: "matching numeric types".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct MultiplyFunc;

impl ExprFunction for MultiplyFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "multiply"
    }

    fn can_fail(&self) -> bool {
        false
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_numeric_args("multiply", &args[0].data_type, &args[1].data_type)?;
        let result =
            arithmetic_result_type(ArithmeticOp::Multiply, &args[0], &args[1]).map_err(|e| {
                FuncError::TypeMismatch {
                    function: "multiply".to_owned(),
                    expected: "numeric".to_owned(),
                    actual: e.to_string(),
                }
            })?;
        Ok(NullableExprType { nullable, ..result })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        match (widen_numeric(args.take(0)), widen_numeric(args.take(1))) {
            (Value::Int64(x), Value::Int64(y)) => match x.checked_mul(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) * BigInt::from(y))),
            },
            (Value::Float64(x), Value::Float64(y)) => Ok(Value::Float64(x * y)),
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x * y)),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x * BigInt::from(y))),
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) * y)),
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x * y)),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x * BigDecimal::from(y))),
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) * y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "multiply".to_owned(),
                expected: "matching numeric types".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct DivideFunc;

impl ExprFunction for DivideFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "divide"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_numeric_args("divide", &args[0].data_type, &args[1].data_type)?;
        let result =
            arithmetic_result_type(ArithmeticOp::Divide, &args[0], &args[1]).map_err(|e| {
                FuncError::TypeMismatch {
                    function: "divide".to_owned(),
                    expected: "numeric".to_owned(),
                    actual: e.to_string(),
                }
            })?;
        Ok(NullableExprType { nullable, ..result })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        match (widen_numeric(args.take(0)), widen_numeric(args.take(1))) {
            (Value::Int64(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::Int64(x), Value::Int64(y)) => match x.checked_div(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) / BigInt::from(y))),
            },
            (Value::Float64(x), Value::Float64(y)) => {
                if y == 0.0 {
                    return Err(FuncError::DivisionByZero);
                }
                Ok(Value::Float64(x / y))
            }
            (Value::BigInt(_), Value::BigInt(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x / y)),
            (Value::BigInt(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x / BigInt::from(y))),
            (Value::Int64(_), Value::BigInt(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) / y)),
            (Value::Decimal(_), Value::Decimal(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x / y)),
            (Value::Decimal(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x / BigDecimal::from(y))),
            (Value::Int64(_), Value::Decimal(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) / y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "divide".to_owned(),
                expected: "matching numeric types".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct ModuloFunc;

impl ExprFunction for ModuloFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "modulo"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        validate_numeric_args("modulo", &args[0].data_type, &args[1].data_type)?;
        let result =
            arithmetic_result_type(ArithmeticOp::Modulo, &args[0], &args[1]).map_err(|e| {
                FuncError::TypeMismatch {
                    function: "modulo".to_owned(),
                    expected: "numeric".to_owned(),
                    actual: e.to_string(),
                }
            })?;
        Ok(NullableExprType { nullable, ..result })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() {
            return Ok(Value::Null);
        }
        match (widen_numeric(args.take(0)), widen_numeric(args.take(1))) {
            (Value::Int64(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::Int64(x), Value::Int64(y)) => match x.checked_rem(y) {
                Some(result) => Ok(Value::Int64(result)),
                // i64::MIN % -1 overflows; promote to BigInt (result is 0).
                None => Ok(Value::BigInt(BigInt::from(x) % BigInt::from(y))),
            },
            (Value::Float64(x), Value::Float64(y)) => {
                if y == 0.0 {
                    return Err(FuncError::DivisionByZero);
                }
                Ok(Value::Float64(x % y))
            }
            (Value::BigInt(_), Value::BigInt(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x % y)),
            (Value::BigInt(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x % BigInt::from(y))),
            (Value::Int64(_), Value::BigInt(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) % y)),
            (Value::Decimal(_), Value::Decimal(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x % y)),
            (Value::Decimal(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x % BigDecimal::from(y))),
            (Value::Int64(_), Value::Decimal(ref y)) if y.is_zero() => {
                Err(FuncError::DivisionByZero)
            }
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) % y)),
            (a, b) => Err(FuncError::TypeMismatch {
                function: "modulo".to_owned(),
                expected: "matching numeric types".to_owned(),
                actual: format!("{:?}, {:?}", a.data_type(), b.data_type()),
            }),
        }
    }
}

struct NegateFunc;

impl ExprFunction for NegateFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "negate"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        // `negate` preserves the input type for every *signed* numeric (a signed
        // `MIN` that overflows its own width promotes to `BigInt` at runtime, just
        // like `abs`). Unsigned integers are the exception: the negation of a
        // non-zero unsigned value is negative and CANNOT be represented in the
        // unsigned type, so preserving the type would be unsound. They widen to the
        // smallest signed type that holds every negated value:
        //   UInt8/16/32 → Int64  (−(2^32−1) fits an `i64`)
        //   UInt64      → BigInt  (−(2^64−1) does not fit any fixed-width int)
        // `resolve_type` and `evaluate` must agree on this; see `evaluate`.
        let arg = &args[0];
        let widened = match arg.data_type {
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                let target = if matches!(arg.data_type, DataType::UInt64) {
                    DataType::BigInt { width: None }
                } else {
                    DataType::Int64
                };
                NullableExprType::new(target, arg.nullable)
            }
            _ => arg.clone(),
        };
        Ok(widened)
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        // Signed narrow / `Int64`: `checked_neg`, promoting to `BigInt` only on the
        // `MIN` overflow (mirrors `abs`). The result type stays the operand's type,
        // matching `resolve_type`.
        match a {
            Value::Int8(x) => match x.checked_neg() {
                Some(result) => return Ok(Value::Int8(result)),
                None => return Ok(Value::BigInt(-BigInt::from(*x))),
            },
            Value::Int16(x) => match x.checked_neg() {
                Some(result) => return Ok(Value::Int16(result)),
                None => return Ok(Value::BigInt(-BigInt::from(*x))),
            },
            Value::Int32(x) => match x.checked_neg() {
                Some(result) => return Ok(Value::Int32(result)),
                None => return Ok(Value::BigInt(-BigInt::from(*x))),
            },
            Value::Int64(x) => match x.checked_neg() {
                Some(result) => return Ok(Value::Int64(result)),
                None => return Ok(Value::BigInt(-BigInt::from(*x))),
            },
            // Unsigned: widen to the signed form `resolve_type` promises.
            // −(any UInt8/16/32) fits an `i64`; UInt64 needs `BigInt`.
            Value::UInt8(x) => return Ok(Value::Int64(-i64::from(*x))),
            Value::UInt16(x) => return Ok(Value::Int64(-i64::from(*x))),
            Value::UInt32(x) => return Ok(Value::Int64(-i64::from(*x))),
            Value::UInt64(x) => return Ok(Value::BigInt(-BigInt::from(*x))),
            Value::Float32(x) => return Ok(Value::Float32(-*x)),
            Value::Float64(x) => return Ok(Value::Float64(-*x)),
            _ => {}
        }
        match args.take(0) {
            Value::BigInt(x) => Ok(Value::BigInt(-x)),
            Value::Decimal(x) => Ok(Value::Decimal(-x)),
            other => Err(FuncError::TypeMismatch {
                function: "negate".to_owned(),
                expected: "numeric".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct AbsFunc;

impl ExprFunction for AbsFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "abs"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(args[0].clone())
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        // `abs` preserves the operand's type. A signed integer's `MIN` has no
        // representable absolute value in its own width, so — like the `Int64`
        // arm — it promotes to `BigInt`. Unsigned integers are already
        // non-negative, so `abs` is the identity.
        match a {
            Value::Int8(x) => match x.checked_abs() {
                Some(result) => return Ok(Value::Int8(result)),
                None => return Ok(Value::BigInt(BigInt::from(*x).magnitude().clone().into())),
            },
            Value::Int16(x) => match x.checked_abs() {
                Some(result) => return Ok(Value::Int16(result)),
                None => return Ok(Value::BigInt(BigInt::from(*x).magnitude().clone().into())),
            },
            Value::Int32(x) => match x.checked_abs() {
                Some(result) => return Ok(Value::Int32(result)),
                None => return Ok(Value::BigInt(BigInt::from(*x).magnitude().clone().into())),
            },
            Value::Int64(x) => match x.checked_abs() {
                Some(result) => return Ok(Value::Int64(result)),
                None => return Ok(Value::BigInt(BigInt::from(*x).magnitude().clone().into())),
            },
            Value::UInt8(_) | Value::UInt16(_) | Value::UInt32(_) | Value::UInt64(_) => {
                return Ok(a.clone());
            }
            Value::Float32(x) => return Ok(Value::Float32(x.abs())),
            Value::Float64(x) => return Ok(Value::Float64(x.abs())),
            _ => {}
        }
        match args.take(0) {
            Value::BigInt(x) => {
                if x < BigInt::zero() {
                    Ok(Value::BigInt(-x))
                } else {
                    Ok(Value::BigInt(x))
                }
            }
            Value::Decimal(x) => Ok(Value::Decimal(x.abs())),
            other => Err(FuncError::TypeMismatch {
                function: "abs".to_owned(),
                expected: "numeric".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct CeilFunc;

impl ExprFunction for CeilFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "ceil"
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
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        // `ceil` resolves to `Int64`. An integer operand is already integral, so
        // return it exactly (no `f64` round-trip — large `Int64`s past 2^53 lose
        // precision through `f64`). Float / `Decimal` operands take the ceiling via
        // the shared `value_to_f64`, which accepts every numeric variant.
        round_to_int64(a, "ceil", f64::ceil, RoundingMode::Ceiling)
    }
}

struct FloorFunc;

impl ExprFunction for FloorFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "floor"
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
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        round_to_int64(a, "floor", f64::floor, RoundingMode::Floor)
    }
}

struct RoundFunc;

impl ExprFunction for RoundFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "round"
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
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        round_to_int64(a, "round", f64::round, RoundingMode::HalfUp)
    }
}

struct MinFunc;

impl ExprFunction for MinFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "min"
    }

    fn can_fail(&self) -> bool {
        false
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        comparable_join(args, "min").map_err(|e| FuncError::TypeMismatch {
            function: "min".to_owned(),
            expected: "arguments sharing one comparable type".to_owned(),
            actual: e.to_string(),
        })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let collected: FuncArgVec = (0..args.len()).map(|i| args.take(i)).collect();
        fold_extremum(collected, "min", Ordering::Less)
    }
}

struct MaxFunc;

impl ExprFunction for MaxFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "max"
    }

    fn can_fail(&self) -> bool {
        false
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        comparable_join(args, "max").map_err(|e| FuncError::TypeMismatch {
            function: "max".to_owned(),
            expected: "arguments sharing one comparable type".to_owned(),
            actual: e.to_string(),
        })
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let collected: FuncArgVec = (0..args.len()).map(|i| args.take(i)).collect();
        fold_extremum(collected, "max", Ordering::Greater)
    }
}

/// Shared `min`/`max` reduction. Skips NULLs (SQL semantics), then keeps the
/// element that compares `winning` against the running best via the
/// cross-numeric [`compare_values`] total order. Because that order is
/// consistent across all numeric widths, the chosen element is the global
/// extremum regardless of how the arguments are grouped — so the reduction is
/// associative and the optimizer may freely flatten nested `min`/`max`.
///
/// `NaN` has no ordering, so it propagates: any `NaN` argument makes the
/// result `NaN` (also order-independent, so associativity holds). A
/// comparison undefined for a non-`NaN` reason (incompatible categories,
/// which `resolve_type` already rejects) is a defensive type error.
fn fold_extremum(args: FuncArgVec, op: &str, winning: Ordering) -> Result<Value, FuncError> {
    let mut best: Option<Value> = None;
    for value in args {
        if value.is_null() {
            continue;
        }
        best = Some(match best {
            None => value,
            Some(current) => match compare_values(&value, &current) {
                Some(order) if order == winning => value,
                Some(_) => current,
                None if is_nan(&value) || is_nan(&current) => Value::Float64(f64::NAN),
                None => {
                    return Err(FuncError::TypeMismatch {
                        function: op.to_owned(),
                        expected: "values with a defined ordering".to_owned(),
                        actual: format!("{:?} vs {:?}", value.data_type(), current.data_type()),
                    });
                }
            },
        });
    }
    Ok(best.unwrap_or(Value::Null))
}

fn is_nan(value: &Value) -> bool {
    match value {
        Value::Float64(f) => f.is_nan(),
        Value::Float32(f) => f.is_nan(),
        _ => false,
    }
}

struct SignFunc;

impl ExprFunction for SignFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sign"
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
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        // `sign` resolves to `Int64` (-1 / 0 / 1) for every numeric variant.
        // Integers use their native `signum`; unsigned integers are 0 or 1.
        // Float / `Decimal` compute the sign by comparison.
        match a {
            Value::Int8(x) => Ok(Value::Int64(i64::from(x.signum()))),
            Value::Int16(x) => Ok(Value::Int64(i64::from(x.signum()))),
            Value::Int32(x) => Ok(Value::Int64(i64::from(x.signum()))),
            Value::Int64(x) => Ok(Value::Int64(x.signum())),
            Value::UInt8(x) => Ok(Value::Int64(i64::from(*x != 0))),
            Value::UInt16(x) => Ok(Value::Int64(i64::from(*x != 0))),
            Value::UInt32(x) => Ok(Value::Int64(i64::from(*x != 0))),
            Value::UInt64(x) => Ok(Value::Int64(i64::from(*x != 0))),
            Value::BigInt(x) => {
                use num_bigint::Sign;
                let s = match x.sign() {
                    Sign::Plus => 1i64,
                    Sign::Minus => -1i64,
                    Sign::NoSign => 0i64,
                };
                Ok(Value::Int64(s))
            }
            Value::Decimal(x) => {
                use num_traits::Signed;
                let s = x.signum();
                if s.is_zero() {
                    Ok(Value::Int64(0))
                } else if s.is_positive() {
                    Ok(Value::Int64(1))
                } else {
                    Ok(Value::Int64(-1))
                }
            }
            Value::Float32(x) => Ok(Value::Int64(float_sign(f64::from(*x)))),
            Value::Float64(x) => Ok(Value::Int64(float_sign(*x))),
            other => Err(FuncError::TypeMismatch {
                function: "sign".to_owned(),
                expected: "numeric".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct PowerFunc;

impl ExprFunction for PowerFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "power"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Float64, nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        let b = args.read(1);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        // `power` always resolves to `Float64`, so it evaluates as float
        // exponentiation for every input — type and value agree. Integer-base,
        // constant-non-negative-exponent powers are lowered to exact integer
        // multiplication by the optimizer before they reach here; what survives as
        // a `power` call (dynamic, negative, or fractional exponents, or a float
        // base) is genuinely `Float64`.
        let base = to_f64(a, "power")?;
        let exp = to_f64(b, "power")?;
        Ok(Value::Float64(base.powf(exp)))
    }
}

struct SqrtFunc;

impl ExprFunction for SqrtFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sqrt"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Float64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let a = args.read(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(a, "sqrt")?;
        Ok(Value::Float64(x.sqrt()))
    }
}

fn to_f64(val: &Value, func_name: &str) -> Result<f64, FuncError> {
    crate::arithmetic_utils::value_to_f64(val, func_name)
}

/// Shared `ceil`/`floor`/`round` body: all three resolve to `Int64`, accept any
/// numeric variant, and differ only in the rounding step — passed both as an
/// `f64` rounder (for `Float32`/`Float64`) and as a [`RoundingMode`] (for the
/// exact `Decimal` path).
///
/// Integer operands (narrow/unsigned/`Int64`/`BigInt`) are already integral, so
/// the rounding direction is a no-op; they are converted to `Int64` *exactly*,
/// bypassing the lossy `f64` path that would round-trip a large `Int64`
/// (magnitude past 2^53) to a wrong value. `Decimal` rounds exactly via
/// `with_scale_round(0, mode)` then converts to `Int64`. Float operands round via
/// `value_to_f64` then cast to `Int64`, range-checked first.
///
/// Any operand (`UInt64`, `BigInt`, `Decimal`, or a float) whose rounded integral
/// value exceeds the `i64` range is an [`FuncError::IntegerOverflow`] — never a
/// silent saturating cast.
fn round_to_int64(
    val: &Value,
    func_name: &str,
    rounder: fn(f64) -> f64,
    mode: RoundingMode,
) -> Result<Value, FuncError> {
    match val {
        Value::Int8(x) => Ok(Value::Int64(i64::from(*x))),
        Value::Int16(x) => Ok(Value::Int64(i64::from(*x))),
        Value::Int32(x) => Ok(Value::Int64(i64::from(*x))),
        Value::Int64(x) => Ok(Value::Int64(*x)),
        Value::UInt8(x) => Ok(Value::Int64(i64::from(*x))),
        Value::UInt16(x) => Ok(Value::Int64(i64::from(*x))),
        Value::UInt32(x) => Ok(Value::Int64(i64::from(*x))),
        Value::UInt64(x) => i64::try_from(*x)
            .map(Value::Int64)
            .map_err(|_| FuncError::IntegerOverflow),
        Value::BigInt(x) => x
            .to_i64()
            .map(Value::Int64)
            .ok_or(FuncError::IntegerOverflow),
        Value::Decimal(x) => {
            // Round exactly to an integer in the requested direction, then convert.
            let rounded = x.with_scale_round(0, mode);
            rounded
                .to_i64()
                .map(Value::Int64)
                .ok_or(FuncError::IntegerOverflow)
        }
        _ => {
            // Float32 / Float64 (every other numeric is handled above).
            let rounded = rounder(to_f64(val, func_name)?);
            // `as i64` saturates; reject out-of-range explicitly to match the
            // exact arms above. (`>=` upper bound: `i64::MAX` is not exactly
            // representable in `f64`, so the nearest `f64` is `2^63`, which is one
            // past the range.)
            if rounded < i64::MIN as f64 || rounded >= 9_223_372_036_854_775_808.0 {
                return Err(FuncError::IntegerOverflow);
            }
            Ok(Value::Int64(rounded as i64))
        }
    }
}

/// Float `sign`: -1 / 0 / 1. A `NaN` is neither `> 0` nor `< 0`, so it yields 0
/// (matching the previous `Float64` behaviour, which fell through to the `else`).
fn float_sign(x: f64) -> i64 {
    if x > 0.0 {
        1
    } else if x < 0.0 {
        -1
    } else {
        0
    }
}

fn is_numeric_type(dt: &DataType) -> bool {
    matches!(
        dt,
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
            | DataType::Decimal { .. }
    )
}

fn validate_numeric_args(
    function: &str,
    left: &DataType,
    right: &DataType,
) -> Result<(), FuncError> {
    if !is_numeric_type(left) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "numeric".to_owned(),
            actual: format!("{left}"),
        });
    }
    if !is_numeric_type(right) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "numeric".to_owned(),
            actual: format!("{right}"),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    /// Assert both the value AND the exact `Value` variant. `Value`'s `PartialEq`
    /// is cross-numeric (`Int8(5) == Int64(5)`), so a value-only `assert_eq!`
    /// would pass even if `evaluate` returned the wrong variant — which is the
    /// exact property the narrow-type fixes are about. Comparing `data_type()`
    /// pins the canonical type, catching a wrong-variant regression.
    fn assert_value_and_type(got: &Value, want: &Value) {
        assert_eq!(got, want, "value");
        assert_eq!(
            got.data_type(),
            want.data_type(),
            "variant: got {got:?}, want {want:?}"
        );
    }

    #[test]
    fn add_int() {
        let f = AddFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(2), Value::Int64(3)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn add_int_overflow_promotes_to_bigint() {
        let f = AddFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(i64::MAX), Value::Int64(1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::BigInt(BigInt::from(i64::MAX) + BigInt::from(1))
        );
    }

    /// Narrow numeric operands (as a source row carries them — `Int32` for a
    /// PG `INT`, `Float32` for `REAL`, …) are widened to the canonical form
    /// the ops compute on, so arithmetic over real source columns works.
    #[test]
    fn arithmetic_widens_narrow_operands() {
        // Int32 * Int32 → Int64 (the pg→pg compute case).
        assert_value_and_type(
            &eval(
                &MultiplyFunc,
                smallvec::smallvec![Value::Int32(5), Value::Int32(5)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(25),
        );
        // Int16 + Int16 → Int64.
        assert_value_and_type(
            &eval(
                &AddFunc,
                smallvec::smallvec![Value::Int16(100), Value::Int16(28)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(128),
        );
        // Int8 + UInt8 → Int64 (the smallest narrow arms).
        assert_value_and_type(
            &eval(
                &AddFunc,
                smallvec::smallvec![Value::Int8(-5), Value::UInt8(9)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(4),
        );
        // UInt32 + UInt32 → Int64.
        assert_value_and_type(
            &eval(
                &AddFunc,
                smallvec::smallvec![Value::UInt32(2), Value::UInt32(3)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(5),
        );
        // Float32 + Float32 → Float64.
        assert_value_and_type(
            &eval(
                &AddFunc,
                smallvec::smallvec![Value::Float32(1.5), Value::Float32(2.5)],
                &ctx(),
            )
            .unwrap(),
            &Value::Float64(4.0),
        );
    }

    /// A `UInt64` past `i64::MAX` cannot be an `Int64`, so `widen_numeric`
    /// promotes it to `BigInt` — exercising that overflow arm end to end.
    #[test]
    fn arithmetic_widens_large_uint64_to_bigint() {
        let result = eval(
            &AddFunc,
            smallvec::smallvec![Value::UInt64(u64::MAX), Value::Int64(1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::BigInt(BigInt::from(u64::MAX) + BigInt::from(1))
        );
    }

    #[test]
    fn add_text_concat() {
        let f = AddFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Text("hello".into()), Value::Text(" world".into())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("hello world".into()));
    }

    #[test]
    fn add_null_propagation() {
        let f = AddFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Null, Value::Int64(3)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn divide_by_zero() {
        let f = DivideFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), Value::Int64(0)],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::DivisionByZero)));
    }

    #[test]
    fn modulo_basic() {
        let f = ModuloFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(7), Value::Int64(3)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn negate_int() {
        let f = NegateFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(-5));
    }

    #[test]
    fn abs_negative() {
        let f = AbsFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(-7)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(7));
    }

    /// `abs` preserves narrow/unsigned/float32 types (a source column arrives as
    /// `Int32` / `UInt32` / `Float32`, not the canonical wide form).
    #[test]
    fn abs_preserves_narrow_and_unsigned_types() {
        let cases = [
            (Value::Int8(-5), Value::Int8(5)),
            (Value::Int16(-300), Value::Int16(300)),
            (Value::Int32(-9), Value::Int32(9)),
            (Value::UInt8(7), Value::UInt8(7)),
            (Value::UInt32(42), Value::UInt32(42)),
            (Value::Float32(-2.5), Value::Float32(2.5)),
        ];
        for (input, want) in cases {
            assert_value_and_type(
                &eval(&AbsFunc, smallvec::smallvec![input.clone()], &ctx()).unwrap(),
                &want,
            );
        }
    }

    /// A signed integer's `MIN` has no representable absolute value in its own
    /// width, so it promotes to `BigInt` — mirroring the `Int64` arm.
    #[test]
    fn abs_of_narrow_min_promotes_to_bigint() {
        assert_eq!(
            eval(&AbsFunc, smallvec::smallvec![Value::Int8(i8::MIN)], &ctx()).unwrap(),
            Value::BigInt(BigInt::from(128)),
        );
    }

    #[test]
    fn ceil_float() {
        let f = CeilFunc;
        let result = eval(&f, smallvec::smallvec![Value::Float64(2.3)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn floor_float() {
        let f = FloorFunc;
        let result = eval(&f, smallvec::smallvec![Value::Float64(2.7)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn round_float() {
        let f = RoundFunc;
        let result = eval(&f, smallvec::smallvec![Value::Float64(2.5)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn min_ignores_null() {
        let f = MinFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), Value::Null, Value::Int64(2)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn max_all_null() {
        let f = MaxFunc;
        let result = eval(&f, smallvec::smallvec![Value::Null, Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn min_of_non_null_args_resolves_non_null() {
        // `min`/`max` skip NULLs, so with no nullable argument the extremum can
        // never be NULL — the resolved type must be non-nullable.
        let args = [
            NullableExprType::non_null(DataType::Int64),
            NullableExprType::non_null(DataType::Int32),
        ];
        let resolved = MinFunc.resolve_type(&args).unwrap();
        assert!(!resolved.nullable, "min of non-null args is non-null");
        assert_eq!(resolved.data_type, DataType::Int64);
    }

    #[test]
    fn max_with_one_non_null_arg_resolves_non_null() {
        // One non-null argument guarantees a non-null extremum even alongside a
        // nullable one (the nullable arg, if NULL, is skipped).
        let args = [
            NullableExprType::nullable(DataType::Int64),
            NullableExprType::non_null(DataType::Int64),
        ];
        let resolved = MaxFunc.resolve_type(&args).unwrap();
        assert!(!resolved.nullable, "one non-null arg makes max non-null");
    }

    #[test]
    fn min_of_all_nullable_args_resolves_nullable() {
        // Every argument nullable → every-arg-NULL is possible → NULL result.
        let args = [
            NullableExprType::nullable(DataType::Int64),
            NullableExprType::nullable(DataType::Int64),
        ];
        let resolved = MinFunc.resolve_type(&args).unwrap();
        assert!(resolved.nullable, "min of all-nullable args is nullable");
    }

    #[test]
    fn min_spills_past_inline_capacity() {
        // Six arguments exceed `FuncArgVec`'s inline capacity of four, forcing
        // the heap spill: the n-ary `fold_extremum` consumer must return the
        // same result on the spilled buffer as it would inline.
        let f = MinFunc;
        let result = eval(
            &f,
            smallvec::smallvec![
                Value::Int64(9),
                Value::Int64(7),
                Value::Int64(5),
                Value::Int64(3),
                Value::Int64(1),
                Value::Int64(8),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn sign_positive() {
        let f = SignFunc;
        let result = eval(&f, smallvec::smallvec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    /// `ceil`/`floor`/`round` resolve to `Int64` and must accept every numeric
    /// variant a source row carries (narrow/unsigned integers, `Float32`,
    /// `Decimal`) — returning an exact `Int64`, never a runtime `TypeMismatch`.
    #[test]
    fn rounders_accept_narrow_unsigned_and_float32() {
        // Integer operands are already integral → returned exactly as Int64.
        for f in [
            &CeilFunc as &dyn ExprFunction,
            &FloorFunc as &dyn ExprFunction,
            &RoundFunc as &dyn ExprFunction,
        ] {
            assert_value_and_type(
                &eval(f, smallvec::smallvec![Value::Int32(-7)], &ctx()).unwrap(),
                &Value::Int64(-7),
            );
            assert_value_and_type(
                &eval(f, smallvec::smallvec![Value::UInt8(200)], &ctx()).unwrap(),
                &Value::Int64(200),
            );
            assert_value_and_type(
                &eval(f, smallvec::smallvec![Value::Int16(42)], &ctx()).unwrap(),
                &Value::Int64(42),
            );
        }
        // Float32 operands round in the requested direction (Int64 result).
        assert_value_and_type(
            &eval(&CeilFunc, smallvec::smallvec![Value::Float32(2.3)], &ctx()).unwrap(),
            &Value::Int64(3),
        );
        assert_value_and_type(
            &eval(&FloorFunc, smallvec::smallvec![Value::Float32(2.7)], &ctx()).unwrap(),
            &Value::Int64(2),
        );
        assert_value_and_type(
            &eval(&RoundFunc, smallvec::smallvec![Value::Float32(2.5)], &ctx()).unwrap(),
            &Value::Int64(3),
        );
        // Decimal operand rounds exactly in the requested direction.
        assert_value_and_type(
            &eval(
                &CeilFunc,
                smallvec::smallvec![Value::Decimal("3.1".parse().unwrap())],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(4),
        );
        assert_value_and_type(
            &eval(
                &FloorFunc,
                smallvec::smallvec![Value::Decimal("3.9".parse().unwrap())],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(3),
        );
        assert_value_and_type(
            &eval(
                &RoundFunc,
                smallvec::smallvec![Value::Decimal("2.5".parse().unwrap())],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(3),
        );
    }

    /// The overflow arms of `ceil`/`floor`/`round`: a `BigInt`, a `UInt64`, or a
    /// `Decimal` whose rounded integral value exceeds the `i64` range, plus an
    /// out-of-range float — all must be a hard `IntegerOverflow`, never a silent
    /// saturating cast to `i64::MAX`.
    #[test]
    fn rounders_reject_out_of_i64_range() {
        let big = Value::BigInt(BigInt::from(i64::MAX) + BigInt::from(1));
        let huge_uint = Value::UInt64(u64::MAX);
        let huge_dec = Value::Decimal("1e30".parse().unwrap());
        let huge_float = Value::Float64(1e30);
        for f in [
            &CeilFunc as &dyn ExprFunction,
            &FloorFunc as &dyn ExprFunction,
            &RoundFunc as &dyn ExprFunction,
        ] {
            for operand in [&big, &huge_uint, &huge_dec, &huge_float] {
                let result = eval(f, smallvec::smallvec![operand.clone()], &ctx());
                assert!(
                    matches!(result, Err(FuncError::IntegerOverflow)),
                    "{}({operand:?}) = {result:?}",
                    f.name()
                );
            }
        }
    }

    /// A large `Int64` (magnitude past 2^53) must round-trip *exactly*: the
    /// integer fast-path returns it verbatim instead of degrading through `f64`.
    #[test]
    fn rounders_preserve_large_int64_exactly() {
        let big = 9_007_199_254_740_993i64; // 2^53 + 1, not representable in f64
        assert_value_and_type(
            &eval(&RoundFunc, smallvec::smallvec![Value::Int64(big)], &ctx()).unwrap(),
            &Value::Int64(big),
        );
    }

    /// `sign` resolves to `Int64` (-1/0/1) and accepts every numeric variant.
    #[test]
    fn sign_accepts_narrow_unsigned_and_float32() {
        assert_value_and_type(
            &eval(&SignFunc, smallvec::smallvec![Value::Int16(-300)], &ctx()).unwrap(),
            &Value::Int64(-1),
        );
        assert_value_and_type(
            &eval(&SignFunc, smallvec::smallvec![Value::Int8(0)], &ctx()).unwrap(),
            &Value::Int64(0),
        );
        // Unsigned is never negative → 0 or 1.
        assert_value_and_type(
            &eval(&SignFunc, smallvec::smallvec![Value::UInt32(42)], &ctx()).unwrap(),
            &Value::Int64(1),
        );
        assert_value_and_type(
            &eval(&SignFunc, smallvec::smallvec![Value::UInt8(0)], &ctx()).unwrap(),
            &Value::Int64(0),
        );
        assert_value_and_type(
            &eval(&SignFunc, smallvec::smallvec![Value::Float32(-2.5)], &ctx()).unwrap(),
            &Value::Int64(-1),
        );
    }

    /// `sign(NaN)` is neither `> 0` nor `< 0`, so the documented `float_sign`
    /// NaN→0 arm yields `Int64(0)`.
    #[test]
    fn sign_of_nan_is_zero() {
        assert_value_and_type(
            &eval(
                &SignFunc,
                smallvec::smallvec![Value::Float64(f64::NAN)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(0),
        );
        assert_value_and_type(
            &eval(
                &SignFunc,
                smallvec::smallvec![Value::Float32(f32::NAN)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int64(0),
        );
    }

    /// `negate` preserves the input type for signed narrow / float32 operands.
    #[test]
    fn negate_preserves_signed_narrow_and_float32() {
        assert_value_and_type(
            &eval(&NegateFunc, smallvec::smallvec![Value::Int8(5)], &ctx()).unwrap(),
            &Value::Int8(-5),
        );
        assert_value_and_type(
            &eval(&NegateFunc, smallvec::smallvec![Value::Int16(-300)], &ctx()).unwrap(),
            &Value::Int16(300),
        );
        assert_value_and_type(
            &eval(&NegateFunc, smallvec::smallvec![Value::Int32(9)], &ctx()).unwrap(),
            &Value::Int32(-9),
        );
        assert_value_and_type(
            &eval(
                &NegateFunc,
                smallvec::smallvec![Value::Float32(2.5)],
                &ctx(),
            )
            .unwrap(),
            &Value::Float32(-2.5),
        );
    }

    /// A signed integer's `MIN` has no representable negation in its own width, so
    /// `negate` promotes it to `BigInt` — mirroring `abs`. Covers every signed
    /// narrow width (Int8/16/32) plus Int64.
    #[test]
    fn negate_of_narrow_min_promotes_to_bigint() {
        assert_value_and_type(
            &eval(
                &NegateFunc,
                smallvec::smallvec![Value::Int8(i8::MIN)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(BigInt::from(128)),
        );
        assert_value_and_type(
            &eval(
                &NegateFunc,
                smallvec::smallvec![Value::Int16(i16::MIN)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(BigInt::from(32_768)),
        );
        assert_value_and_type(
            &eval(
                &NegateFunc,
                smallvec::smallvec![Value::Int32(i32::MIN)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(BigInt::from(2_147_483_648i64)),
        );
        assert_value_and_type(
            &eval(
                &NegateFunc,
                smallvec::smallvec![Value::Int64(i64::MIN)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(-BigInt::from(i64::MIN)),
        );
    }

    /// Same MIN-overflow promotion for `abs` across every signed narrow width
    /// (the existing suite only covered Int8 MIN).
    #[test]
    fn abs_of_narrow_min_promotes_to_bigint_all_widths() {
        assert_value_and_type(
            &eval(
                &AbsFunc,
                smallvec::smallvec![Value::Int16(i16::MIN)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(BigInt::from(32_768)),
        );
        assert_value_and_type(
            &eval(
                &AbsFunc,
                smallvec::smallvec![Value::Int32(i32::MIN)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(BigInt::from(2_147_483_648i64)),
        );
    }

    /// Negating an unsigned value is negative and cannot live in an unsigned type.
    /// `resolve_type` widens UInt8/16/32 → Int64 (UInt64 → BigInt), and `evaluate`
    /// must produce exactly that type so downstream coercion stays sound.
    #[test]
    fn negate_unsigned_widens_to_signed() {
        // UInt8/16/32 → Int64 value.
        assert_value_and_type(
            &eval(&NegateFunc, smallvec::smallvec![Value::UInt8(5)], &ctx()).unwrap(),
            &Value::Int64(-5),
        );
        assert_value_and_type(
            &eval(&NegateFunc, smallvec::smallvec![Value::UInt32(7)], &ctx()).unwrap(),
            &Value::Int64(-7),
        );
        // UInt64 → BigInt value (−(2^64−1) does not fit any fixed-width int).
        assert_value_and_type(
            &eval(
                &NegateFunc,
                smallvec::smallvec![Value::UInt64(u64::MAX)],
                &ctx(),
            )
            .unwrap(),
            &Value::BigInt(-BigInt::from(u64::MAX)),
        );
        // resolve_type agrees with the produced value type.
        let t8 = NegateFunc
            .resolve_type(&[NullableExprType::non_null(DataType::UInt8)])
            .unwrap();
        assert_eq!(t8.data_type, DataType::Int64);
        let t64 = NegateFunc
            .resolve_type(&[NullableExprType::non_null(DataType::UInt64)])
            .unwrap();
        assert_eq!(t64.data_type, DataType::BigInt { width: None });
    }

    /// `min`/`max` reduce via the cross-numeric `compare_values`, so they already
    /// accept narrow/unsigned/float32 operands and return the winning value
    /// unchanged (no widening). Regression guard for that path.
    #[test]
    fn min_max_accept_narrow_unsigned_float32() {
        // The winning operand is returned with its ORIGINAL narrow variant intact
        // (no widening) — `assert_value_and_type` pins that via `data_type()`.
        assert_value_and_type(
            &eval(
                &MinFunc,
                smallvec::smallvec![Value::Int16(5), Value::UInt8(2), Value::Int32(9)],
                &ctx(),
            )
            .unwrap(),
            &Value::UInt8(2),
        );
        assert_value_and_type(
            &eval(
                &MaxFunc,
                smallvec::smallvec![Value::Float32(1.5), Value::Int32(3)],
                &ctx(),
            )
            .unwrap(),
            &Value::Int32(3),
        );
    }

    #[test]
    fn power_basic() {
        let f = PowerFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Float64(2.0), Value::Float64(3.0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(8.0));
    }

    #[test]
    fn power_of_integer_args_evaluates_as_float() {
        // The runtime `power` is Float64-only — it always resolves to Float64, so
        // type and value agree. Exact integer powers are produced by the
        // optimizer's lowering to multiplication, never here.
        let f = PowerFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(2), Value::Int64(10)],
            &ctx(),
        )
        .unwrap();
        // Assert the VARIANT, not just the value: `Value` equality is
        // cross-numeric (`Int64(1024) == Float64(1024.0)`), so `assert_eq!` alone
        // would not catch a regression back to an integer result.
        assert!(
            matches!(result, Value::Float64(f) if f == 1024.0),
            "got {result:?}"
        );
    }

    #[test]
    fn power_negative_exponent_is_float() {
        let f = PowerFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(2), Value::Int64(-1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(0.5));
    }

    #[test]
    fn sqrt_basic() {
        let f = SqrtFunc;
        let result = eval(&f, smallvec::smallvec![Value::Float64(9.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(3.0));
    }

    /// The float-domain builtins accept narrow/unsigned integer operands via the
    /// shared `value_to_f64` — a source `Int32` column flows into `sqrt` without a
    /// runtime `TypeMismatch`.
    #[test]
    fn sqrt_accepts_narrow_integer_operands() {
        assert_eq!(
            eval(&SqrtFunc, smallvec::smallvec![Value::Int32(4)], &ctx()).unwrap(),
            Value::Float64(2.0)
        );
        assert_eq!(
            eval(&SqrtFunc, smallvec::smallvec![Value::UInt16(9)], &ctx()).unwrap(),
            Value::Float64(3.0)
        );
    }

    // --- BigInt and Decimal tests ---

    #[test]
    fn add_bigint() {
        let f = AddFunc;
        let a = Value::BigInt(BigInt::from(1_000_000_000_000i64));
        let b = Value::BigInt(BigInt::from(2_000_000_000_000i64));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(3_000_000_000_000i64)));
    }

    #[test]
    fn add_bigint_and_int64() {
        let f = AddFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::Int64(50);
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(150)));
    }

    #[test]
    fn add_decimal() {
        let f = AddFunc;
        let a = Value::Decimal("1.5".parse().unwrap());
        let b = Value::Decimal("2.5".parse().unwrap());
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::Decimal("4.0".parse().unwrap()));
    }

    #[test]
    fn subtract_bigint() {
        let f = SubtractFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(30));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(70)));
    }

    #[test]
    fn multiply_bigint() {
        let f = MultiplyFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(200));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(20_000)));
    }

    #[test]
    fn divide_bigint() {
        let f = DivideFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(4));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(25)));
    }

    #[test]
    fn divide_bigint_by_zero() {
        let f = DivideFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(0));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx());
        assert!(matches!(result, Err(FuncError::DivisionByZero)));
    }

    #[test]
    fn modulo_bigint() {
        let f = ModuloFunc;
        let a = Value::BigInt(BigInt::from(17));
        let b = Value::BigInt(BigInt::from(5));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(2)));
    }

    #[test]
    fn negate_bigint() {
        let f = NegateFunc;
        let a = Value::BigInt(BigInt::from(42));
        let result = eval(&f, smallvec::smallvec![a], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(-42)));
    }

    #[test]
    fn abs_bigint_negative() {
        let f = AbsFunc;
        let a = Value::BigInt(BigInt::from(-99));
        let result = eval(&f, smallvec::smallvec![a], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(99)));
    }

    #[test]
    fn abs_decimal_negative() {
        let f = AbsFunc;
        let a = Value::Decimal("-3.14".parse().unwrap());
        let result = eval(&f, smallvec::smallvec![a], &ctx()).unwrap();
        assert_eq!(result, Value::Decimal("3.14".parse().unwrap()));
    }

    #[test]
    fn sign_bigint_negative() {
        let f = SignFunc;
        let a = Value::BigInt(BigInt::from(-42));
        let result = eval(&f, smallvec::smallvec![a], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(-1));
    }

    #[test]
    fn sign_decimal_positive() {
        let f = SignFunc;
        let a = Value::Decimal("7.5".parse().unwrap());
        let result = eval(&f, smallvec::smallvec![a], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn min_bigint() {
        let f = MinFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(50));
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(50)));
    }

    #[test]
    fn max_decimal() {
        let f = MaxFunc;
        let a = Value::Decimal("1.5".parse().unwrap());
        let b = Value::Decimal("2.7".parse().unwrap());
        let result = eval(&f, smallvec::smallvec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::Decimal("2.7".parse().unwrap()));
    }

    #[test]
    fn min_mixed_int_and_float_picks_true_minimum() {
        // The float 1.0 is the global minimum across mixed numeric types.
        let f = MinFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), Value::Float64(1.0), Value::Int64(2)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Float64(1.0));
    }

    #[test]
    fn min_is_associative_under_regrouping() {
        // Regression for the flatten bug: nested and flat grouping must agree.
        let f = MinFunc;
        let flat = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), Value::Float64(1.0), Value::Int64(2)],
            &ctx(),
        )
        .unwrap();
        let nested_inner = eval(
            &f,
            smallvec::smallvec![Value::Float64(1.0), Value::Int64(2)],
            &ctx(),
        )
        .unwrap();
        let nested = eval(
            &f,
            smallvec::smallvec![Value::Int64(5), nested_inner],
            &ctx(),
        )
        .unwrap();
        assert_eq!(flat, nested);
    }

    #[test]
    fn min_propagates_nan() {
        // NaN has no defined ordering, so it propagates to the result.
        let f = MinFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Float64(1.0), Value::Float64(f64::NAN)],
            &ctx(),
        )
        .unwrap();
        assert!(matches!(result, Value::Float64(n) if n.is_nan()));
    }

    #[test]
    fn max_nan_propagates_regardless_of_position() {
        // NaN anywhere in the arguments wins — order-independent (associative).
        let f = MaxFunc;
        let first = eval(
            &f,
            smallvec::smallvec![Value::Float64(f64::NAN), Value::Float64(2.0)],
            &ctx(),
        )
        .unwrap();
        let last = eval(
            &f,
            smallvec::smallvec![Value::Float64(2.0), Value::Float64(f64::NAN)],
            &ctx(),
        )
        .unwrap();
        assert!(matches!(first, Value::Float64(n) if n.is_nan()));
        assert!(matches!(last, Value::Float64(n) if n.is_nan()));
    }

    #[test]
    fn max_resolve_type_rejects_mixed_categories() {
        // max over text and a number is undefined for ordering — reject at
        // type-resolution time.
        let f = MaxFunc;
        let args = [
            NullableExprType::new(DataType::text(), false),
            NullableExprType::new(DataType::Float64, false),
        ];
        assert!(matches!(
            f.resolve_type(&args),
            Err(FuncError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn min_resolve_type_widens_mixed_numerics_to_float() {
        let f = MinFunc;
        let args = [
            NullableExprType::new(DataType::Int64, false),
            NullableExprType::new(DataType::Float64, false),
        ];
        let resolved = f.resolve_type(&args).unwrap();
        assert_eq!(resolved.data_type, DataType::Float64);
        // Both arguments are non-null, so the extremum is non-null (NULLs are
        // skipped, and there are none to skip here).
        assert!(!resolved.nullable);
    }

    // -----------------------------------------------------------------------
    // Property tests: crash-freedom and arithmetic bounds
    // -----------------------------------------------------------------------

    mod proptests {
        use super::*;
        use crate::registry::FunctionRegistry;
        use crate::signature::ExprFunction;
        use crate::test_support::{ctx, eval};
        use proptest::prelude::*;

        fn arb_numeric_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                any::<i64>().prop_map(Value::Int64),
                any::<f64>().prop_map(Value::Float64),
                any::<i64>().prop_map(|n| Value::BigInt(BigInt::from(n))),
                prop_oneof![
                    Just("0"),
                    Just("1.5"),
                    Just("-999999999999999999.123456789"),
                    Just("0.000000001"),
                ]
                .prop_map(|s| Value::Decimal(s.parse().unwrap())),
            ]
        }

        /// Numeric values biased toward edge cases that commonly trigger
        /// overflow, division-by-zero, and special-float behaviour.
        fn arb_edge_numeric_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                prop_oneof![
                    Just(Value::Int64(0)),
                    Just(Value::Int64(1)),
                    Just(Value::Int64(-1)),
                    Just(Value::Int64(i64::MIN)),
                    Just(Value::Int64(i64::MAX)),
                    any::<i64>().prop_map(Value::Int64),
                ],
                prop_oneof![
                    Just(Value::Float64(0.0)),
                    Just(Value::Float64(-0.0)),
                    Just(Value::Float64(f64::NAN)),
                    Just(Value::Float64(f64::INFINITY)),
                    Just(Value::Float64(f64::NEG_INFINITY)),
                    Just(Value::Float64(f64::MIN)),
                    Just(Value::Float64(f64::MAX)),
                    any::<f64>().prop_map(Value::Float64),
                ],
                prop_oneof![
                    Just(Value::BigInt(BigInt::from(0))),
                    Just(Value::BigInt(BigInt::from(i64::MIN))),
                    Just(Value::BigInt(BigInt::from(i64::MAX))),
                    Just(Value::BigInt(
                        BigInt::from(i64::MAX) * BigInt::from(i64::MAX)
                    )),
                    any::<i64>().prop_map(|n| Value::BigInt(BigInt::from(n))),
                ],
                prop_oneof![
                    Just("0"),
                    Just("1.5"),
                    Just("-999999999999999999.123456789"),
                    Just("0.000000001"),
                ]
                .prop_map(|s| Value::Decimal(s.parse().unwrap())),
            ]
        }

        fn eval_binary(func: &dyn ExprFunction, a: Value, b: Value) -> Result<Value, FuncError> {
            eval(func, smallvec::smallvec![a, b], &ctx())
        }

        fn eval_unary(func: &dyn ExprFunction, a: Value) -> Result<Value, FuncError> {
            eval(func, smallvec::smallvec![a], &ctx())
        }

        // --- Property 1: Eval crash-freedom for binary arithmetic ---

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2000))]

            #[test]
            fn add_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&AddFunc, a, b);
            }

            #[test]
            fn subtract_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&SubtractFunc, a, b);
            }

            #[test]
            fn multiply_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&MultiplyFunc, a, b);
            }

            #[test]
            fn divide_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&DivideFunc, a, b);
            }

            #[test]
            fn modulo_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&ModuloFunc, a, b);
            }

            #[test]
            fn power_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&PowerFunc, a, b);
            }
        }

        // --- Property 1b: Unary arithmetic crash-freedom ---

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2000))]

            #[test]
            fn negate_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&NegateFunc, a);
            }

            #[test]
            fn abs_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&AbsFunc, a);
            }

            #[test]
            fn ceil_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&CeilFunc, a);
            }

            #[test]
            fn floor_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&FloorFunc, a);
            }

            #[test]
            fn round_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&RoundFunc, a);
            }

            #[test]
            fn sqrt_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&SqrtFunc, a);
            }

            #[test]
            fn sign_never_panics(a in arb_edge_numeric_value()) {
                let _ = eval_unary(&SignFunc, a);
            }
        }

        // --- Property 1c: Min/Max crash-freedom ---

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(1000))]

            #[test]
            fn min_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&MinFunc, a, b);
            }

            #[test]
            fn max_never_panics(a in arb_edge_numeric_value(), b in arb_edge_numeric_value()) {
                let _ = eval_binary(&MaxFunc, a, b);
            }
        }

        // --- Property 2: Bounds arithmetic ---

        #[test]
        fn int64_max_plus_one_promotes_to_bigint() {
            let result = eval_binary(&AddFunc, Value::Int64(i64::MAX), Value::Int64(1)).unwrap();
            assert_eq!(
                result,
                Value::BigInt(BigInt::from(i64::MAX) + BigInt::from(1))
            );
        }

        #[test]
        fn int64_min_minus_one_promotes_to_bigint() {
            let result =
                eval_binary(&SubtractFunc, Value::Int64(i64::MIN), Value::Int64(1)).unwrap();
            assert_eq!(
                result,
                Value::BigInt(BigInt::from(i64::MIN) - BigInt::from(1))
            );
        }

        #[test]
        fn int64_min_times_neg_one_promotes_to_bigint() {
            let result =
                eval_binary(&MultiplyFunc, Value::Int64(i64::MIN), Value::Int64(-1)).unwrap();
            assert_eq!(
                result,
                Value::BigInt(BigInt::from(i64::MIN) * BigInt::from(-1i64))
            );
        }

        #[test]
        fn int64_division_by_zero_returns_error() {
            let result = eval_binary(&DivideFunc, Value::Int64(42), Value::Int64(0));
            assert!(matches!(result, Err(FuncError::DivisionByZero)));
        }

        #[test]
        fn bigint_division_by_zero_returns_error() {
            let result = eval_binary(
                &DivideFunc,
                Value::BigInt(BigInt::from(42)),
                Value::BigInt(BigInt::from(0)),
            );
            assert!(matches!(result, Err(FuncError::DivisionByZero)));
        }

        #[test]
        fn decimal_division_by_zero_returns_error() {
            let result = eval_binary(
                &DivideFunc,
                Value::Decimal("42.5".parse().unwrap()),
                Value::Decimal("0".parse().unwrap()),
            );
            assert!(matches!(result, Err(FuncError::DivisionByZero)));
        }

        #[test]
        fn float64_division_by_zero_returns_error() {
            let result = eval_binary(&DivideFunc, Value::Float64(42.0), Value::Float64(0.0));
            assert!(matches!(result, Err(FuncError::DivisionByZero)));
        }

        #[test]
        fn int64_min_div_neg_one_promotes_to_bigint() {
            let result =
                eval_binary(&DivideFunc, Value::Int64(i64::MIN), Value::Int64(-1)).unwrap();
            assert_eq!(
                result,
                Value::BigInt(BigInt::from(i64::MIN) / BigInt::from(-1i64))
            );
        }

        #[test]
        fn int64_min_modulo_neg_one_promotes_to_bigint() {
            let result =
                eval_binary(&ModuloFunc, Value::Int64(i64::MIN), Value::Int64(-1)).unwrap();
            assert_eq!(
                result,
                Value::BigInt(BigInt::from(i64::MIN) % BigInt::from(-1i64))
            );
        }

        #[test]
        fn int64_modulo_by_zero_returns_error() {
            let result = eval_binary(&ModuloFunc, Value::Int64(42), Value::Int64(0));
            assert!(matches!(result, Err(FuncError::DivisionByZero)));
        }

        #[test]
        fn int64_max_squared_promotes_to_bigint() {
            let result = eval_binary(&PowerFunc, Value::Int64(i64::MAX), Value::Int64(2)).unwrap();
            assert_eq!(result, Value::BigInt(BigInt::from(i64::MAX).pow(2)));
        }

        #[test]
        fn negate_int64_min_promotes_to_bigint() {
            let result = eval_unary(&NegateFunc, Value::Int64(i64::MIN)).unwrap();
            assert_eq!(result, Value::BigInt(-BigInt::from(i64::MIN)));
        }

        #[test]
        fn abs_int64_min_promotes_to_bigint() {
            let result = eval_unary(&AbsFunc, Value::Int64(i64::MIN)).unwrap();
            let expected = BigInt::from(i64::MIN).magnitude().clone().into();
            assert_eq!(result, Value::BigInt(expected));
        }

        // --- Property 3: Comparison crash-freedom ---

        // Comparison and cast function structs are module-private, so we
        // resolve them through the builtin registry by name.

        use std::sync::LazyLock;

        static REGISTRY: LazyLock<FunctionRegistry> =
            LazyLock::new(FunctionRegistry::with_builtins);

        fn comparison_func(name: &str) -> &'static dyn ExprFunction {
            let r = REGISTRY
                .get_ref(name, Some(2))
                .expect("function must exist");
            REGISTRY.get_by_ref(r)
        }

        fn cast_func(name: &str, arity: usize) -> &'static dyn ExprFunction {
            let r = REGISTRY
                .get_ref(name, Some(arity))
                .expect("function must exist");
            REGISTRY.get_by_ref(r)
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(1000))]

            #[test]
            fn equals_never_panics(a in arb_numeric_value(), b in arb_numeric_value()) {
                let f = comparison_func("equals");
                let _ = eval_binary(f, a, b);
            }

            #[test]
            fn not_equals_never_panics(a in arb_numeric_value(), b in arb_numeric_value()) {
                let f = comparison_func("notEquals");
                let _ = eval_binary(f, a, b);
            }

            #[test]
            fn greater_never_panics(a in arb_numeric_value(), b in arb_numeric_value()) {
                let f = comparison_func("greater");
                let _ = eval_binary(f, a, b);
            }

            #[test]
            fn less_never_panics(a in arb_numeric_value(), b in arb_numeric_value()) {
                let f = comparison_func("less");
                let _ = eval_binary(f, a, b);
            }

            #[test]
            fn greater_or_equals_never_panics(a in arb_numeric_value(), b in arb_numeric_value()) {
                let f = comparison_func("greaterOrEquals");
                let _ = eval_binary(f, a, b);
            }

            #[test]
            fn less_or_equals_never_panics(a in arb_numeric_value(), b in arb_numeric_value()) {
                let f = comparison_func("lessOrEquals");
                let _ = eval_binary(f, a, b);
            }
        }

        // --- Property 4: Cast crash-freedom ---

        /// Broader value generation including non-numeric types for cast tests.
        fn arb_any_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                Just(Value::Null),
                any::<bool>().prop_map(Value::Bool),
                any::<i64>().prop_map(Value::Int64),
                any::<f64>().prop_map(Value::Float64),
                any::<i64>().prop_map(|n| Value::BigInt(BigInt::from(n))),
                prop_oneof![Just("0"), Just("1.5"), Just("-42.0"), Just("0.000000001"),]
                    .prop_map(|s| Value::Decimal(s.parse().unwrap())),
                ".*".prop_map(Value::Text),
            ]
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(1000))]

            #[test]
            fn cast_to_int64_never_panics(a in arb_any_value()) {
                let f = cast_func("toInt64", 1);
                let _ = eval_unary(f, a);
            }

            #[test]
            fn cast_to_float64_never_panics(a in arb_any_value()) {
                let f = cast_func("toFloat64", 1);
                let _ = eval_unary(f, a);
            }

            #[test]
            fn cast_to_bigint_never_panics(a in arb_any_value()) {
                let f = cast_func("toBigInt", 1);
                let _ = eval_unary(f, a);
            }

            #[test]
            fn cast_to_string_never_panics(a in arb_any_value()) {
                let f = cast_func("toStringCast", 1);
                let _ = eval_unary(f, a);
            }

            #[test]
            fn cast_to_bool_never_panics(a in arb_any_value()) {
                let f = cast_func("toBool", 1);
                let _ = eval_unary(f, a);
            }

            #[test]
            fn cast_to_decimal_never_panics(a in arb_any_value()) {
                let f = cast_func("toDecimal", 3);
                let _ = eval(
                    f,
                    smallvec::smallvec![a, Value::Int64(38), Value::Int64(10)],
                    &ctx(),
                );
            }
        }
    }
}
