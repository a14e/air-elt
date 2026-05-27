use air_elt_expr_types::bounds::{ArithmeticOp, arithmetic_result_type, concat_result_type};
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use num_traits::Zero;

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

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
    fn name(&self) -> &str {
        "add"
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Text(mut s), Value::Text(t)) => {
                s.push_str(&t);
                Ok(Value::Text(s))
            }
            (Value::Int64(x), Value::Int64(y)) => match x.checked_add(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) + BigInt::from(y))),
            },
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x + y)),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x + BigInt::from(y))),
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) + y)),
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x + y)),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x + BigDecimal::from(y))),
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) + y)),
            (Value::Float64(x), Value::Float64(y)) => Ok(Value::Float64(x + y)),
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
    fn name(&self) -> &str {
        "subtract"
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Int64(x), Value::Int64(y)) => match x.checked_sub(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) - BigInt::from(y))),
            },
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x - y)),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x - BigInt::from(y))),
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) - y)),
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x - y)),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x - BigDecimal::from(y))),
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) - y)),
            (Value::Float64(x), Value::Float64(y)) => Ok(Value::Float64(x - y)),
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
    fn name(&self) -> &str {
        "multiply"
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Int64(x), Value::Int64(y)) => match x.checked_mul(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) * BigInt::from(y))),
            },
            (Value::BigInt(x), Value::BigInt(y)) => Ok(Value::BigInt(x * y)),
            (Value::BigInt(x), Value::Int64(y)) => Ok(Value::BigInt(x * BigInt::from(y))),
            (Value::Int64(x), Value::BigInt(y)) => Ok(Value::BigInt(BigInt::from(x) * y)),
            (Value::Decimal(x), Value::Decimal(y)) => Ok(Value::Decimal(x * y)),
            (Value::Decimal(x), Value::Int64(y)) => Ok(Value::Decimal(x * BigDecimal::from(y))),
            (Value::Int64(x), Value::Decimal(y)) => Ok(Value::Decimal(BigDecimal::from(x) * y)),
            (Value::Float64(x), Value::Float64(y)) => Ok(Value::Float64(x * y)),
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Int64(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::Int64(x), Value::Int64(y)) => match x.checked_div(y) {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x) / BigInt::from(y))),
            },
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
            (Value::Float64(x), Value::Float64(y)) => {
                if y == 0.0 {
                    return Err(FuncError::DivisionByZero);
                }
                Ok(Value::Float64(x / y))
            }
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        match (a, b) {
            (Value::Int64(_), Value::Int64(0)) => Err(FuncError::DivisionByZero),
            (Value::Int64(x), Value::Int64(y)) => match x.checked_rem(y) {
                Some(result) => Ok(Value::Int64(result)),
                // i64::MIN % -1 overflows; promote to BigInt (result is 0).
                None => Ok(Value::BigInt(BigInt::from(x) % BigInt::from(y))),
            },
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
            (Value::Float64(x), Value::Float64(y)) => {
                if y == 0.0 {
                    return Err(FuncError::DivisionByZero);
                }
                Ok(Value::Float64(x % y))
            }
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
        Ok(args[0].clone())
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Int64(x) => match x.checked_neg() {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(-BigInt::from(x))),
            },
            Value::BigInt(x) => Ok(Value::BigInt(-x)),
            Value::Decimal(x) => Ok(Value::Decimal(-x)),
            Value::Float64(x) => Ok(Value::Float64(-x)),
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Int64(x) => match x.checked_abs() {
                Some(result) => Ok(Value::Int64(result)),
                None => Ok(Value::BigInt(BigInt::from(x).magnitude().clone().into())),
            },
            Value::BigInt(x) => {
                if x < BigInt::zero() {
                    Ok(Value::BigInt(-x))
                } else {
                    Ok(Value::BigInt(x))
                }
            }
            Value::Decimal(x) => Ok(Value::Decimal(x.abs())),
            Value::Float64(x) => Ok(Value::Float64(x.abs())),
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Int64(x) => Ok(Value::Int64(x)),
            Value::Float64(x) => Ok(Value::Int64(x.ceil() as i64)),
            other => Err(FuncError::TypeMismatch {
                function: "ceil".to_owned(),
                expected: "numeric".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct FloorFunc;

impl ExprFunction for FloorFunc {
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Int64(x) => Ok(Value::Int64(x)),
            Value::Float64(x) => Ok(Value::Int64(x.floor() as i64)),
            other => Err(FuncError::TypeMismatch {
                function: "floor".to_owned(),
                expected: "numeric".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct RoundFunc;

impl ExprFunction for RoundFunc {
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Int64(x) => Ok(Value::Int64(x)),
            Value::Float64(x) => Ok(Value::Int64(x.round() as i64)),
            other => Err(FuncError::TypeMismatch {
                function: "round".to_owned(),
                expected: "numeric".to_owned(),
                actual: format!("{:?}", other.data_type()),
            }),
        }
    }
}

struct MinFunc;

impl ExprFunction for MinFunc {
    fn name(&self) -> &str {
        "min"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(args[0].data_type.clone()))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let mut result: Option<Value> = None;
        for val in args {
            if val.is_null() {
                continue;
            }
            result = Some(match result {
                None => val,
                Some(current) => match (&current, &val) {
                    (Value::Int64(a), Value::Int64(b)) => {
                        if b < a {
                            val
                        } else {
                            current
                        }
                    }
                    (Value::BigInt(a), Value::BigInt(b)) => {
                        if b < a {
                            val
                        } else {
                            current
                        }
                    }
                    (Value::Decimal(a), Value::Decimal(b)) => {
                        if b < a {
                            val
                        } else {
                            current
                        }
                    }
                    (Value::Float64(a), Value::Float64(b)) => {
                        if b < a {
                            val
                        } else {
                            current
                        }
                    }
                    _ => current,
                },
            });
        }
        Ok(result.unwrap_or(Value::Null))
    }
}

struct MaxFunc;

impl ExprFunction for MaxFunc {
    fn name(&self) -> &str {
        "max"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(args[0].data_type.clone()))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let mut result: Option<Value> = None;
        for val in args {
            if val.is_null() {
                continue;
            }
            result = Some(match result {
                None => val,
                Some(current) => match (&current, &val) {
                    (Value::Int64(a), Value::Int64(b)) => {
                        if b > a {
                            val
                        } else {
                            current
                        }
                    }
                    (Value::BigInt(a), Value::BigInt(b)) => {
                        if b > a {
                            val
                        } else {
                            current
                        }
                    }
                    (Value::Decimal(a), Value::Decimal(b)) => {
                        if b > a {
                            val
                        } else {
                            current
                        }
                    }
                    (Value::Float64(a), Value::Float64(b)) => {
                        if b > a {
                            val
                        } else {
                            current
                        }
                    }
                    _ => current,
                },
            });
        }
        Ok(result.unwrap_or(Value::Null))
    }
}

struct SignFunc;

impl ExprFunction for SignFunc {
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        match a {
            Value::Int64(x) => Ok(Value::Int64(x.signum())),
            Value::BigInt(ref x) => {
                use num_bigint::Sign;
                let s = match x.sign() {
                    Sign::Plus => 1i64,
                    Sign::Minus => -1i64,
                    Sign::NoSign => 0i64,
                };
                Ok(Value::Int64(s))
            }
            Value::Decimal(ref x) => {
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
            Value::Float64(x) => {
                if x > 0.0 {
                    Ok(Value::Int64(1))
                } else if x < 0.0 {
                    Ok(Value::Int64(-1))
                } else {
                    Ok(Value::Int64(0))
                }
            }
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b = args.remove(1);
        let a = args.remove(0);
        if a.is_null() || b.is_null() {
            return Ok(Value::Null);
        }
        // Integer power: both Int64 -> compute as integer
        if let (Value::Int64(base), Value::Int64(exp)) = (&a, &b) {
            if *exp >= 0 {
                let exp_u32 = (*exp).min(63) as u32;
                match base.checked_pow(exp_u32) {
                    Some(result) => return Ok(Value::Int64(result)),
                    None => {
                        return Ok(Value::BigInt(BigInt::from(*base).pow(exp_u32)));
                    }
                }
            }
            // Negative exponent for integers -> float
            let base_f = *base as f64;
            let exp_f = *exp as f64;
            return Ok(Value::Float64(base_f.powf(exp_f)));
        }
        let base = to_f64(&a, "power")?;
        let exp = to_f64(&b, "power")?;
        Ok(Value::Float64(base.powf(exp)))
    }
}

struct SqrtFunc;

impl ExprFunction for SqrtFunc {
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let a = args.remove(0);
        if a.is_null() {
            return Ok(Value::Null);
        }
        let x = to_f64(&a, "sqrt")?;
        Ok(Value::Float64(x.sqrt()))
    }
}

fn to_f64(val: &Value, func_name: &str) -> Result<f64, FuncError> {
    match val {
        Value::Int64(x) => Ok(*x as f64),
        Value::Float64(x) => Ok(*x),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "numeric".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
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
    use crate::test_support::ctx;

    #[test]
    fn add_int() {
        let f = AddFunc;
        let result = f
            .evaluate(vec![Value::Int64(2), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn add_int_overflow_promotes_to_bigint() {
        let f = AddFunc;
        let result = f
            .evaluate(vec![Value::Int64(i64::MAX), Value::Int64(1)], &ctx())
            .unwrap();
        assert_eq!(
            result,
            Value::BigInt(BigInt::from(i64::MAX) + BigInt::from(1))
        );
    }

    #[test]
    fn add_text_concat() {
        let f = AddFunc;
        let result = f
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text(" world".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("hello world".into()));
    }

    #[test]
    fn add_null_propagation() {
        let f = AddFunc;
        let result = f
            .evaluate(vec![Value::Null, Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn divide_by_zero() {
        let f = DivideFunc;
        let result = f.evaluate(vec![Value::Int64(5), Value::Int64(0)], &ctx());
        assert!(matches!(result, Err(FuncError::DivisionByZero)));
    }

    #[test]
    fn modulo_basic() {
        let f = ModuloFunc;
        let result = f
            .evaluate(vec![Value::Int64(7), Value::Int64(3)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn negate_int() {
        let f = NegateFunc;
        let result = f.evaluate(vec![Value::Int64(5)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(-5));
    }

    #[test]
    fn abs_negative() {
        let f = AbsFunc;
        let result = f.evaluate(vec![Value::Int64(-7)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(7));
    }

    #[test]
    fn ceil_float() {
        let f = CeilFunc;
        let result = f.evaluate(vec![Value::Float64(2.3)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn floor_float() {
        let f = FloorFunc;
        let result = f.evaluate(vec![Value::Float64(2.7)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn round_float() {
        let f = RoundFunc;
        let result = f.evaluate(vec![Value::Float64(2.5)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn min_ignores_null() {
        let f = MinFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Null, Value::Int64(2)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn max_all_null() {
        let f = MaxFunc;
        let result = f.evaluate(vec![Value::Null, Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn sign_positive() {
        let f = SignFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn power_basic() {
        let f = PowerFunc;
        let result = f
            .evaluate(vec![Value::Float64(2.0), Value::Float64(3.0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Float64(8.0));
    }

    #[test]
    fn power_integer_both_int64() {
        let f = PowerFunc;
        let result = f
            .evaluate(vec![Value::Int64(2), Value::Int64(10)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(1024));
    }

    #[test]
    fn power_integer_overflow_to_bigint() {
        let f = PowerFunc;
        let result = f
            .evaluate(vec![Value::Int64(2), Value::Int64(63)], &ctx())
            .unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(2).pow(63)));
    }

    #[test]
    fn power_integer_negative_exponent() {
        let f = PowerFunc;
        let result = f
            .evaluate(vec![Value::Int64(2), Value::Int64(-1)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Float64(0.5));
    }

    #[test]
    fn sqrt_basic() {
        let f = SqrtFunc;
        let result = f.evaluate(vec![Value::Float64(9.0)], &ctx()).unwrap();
        assert_eq!(result, Value::Float64(3.0));
    }

    // --- BigInt and Decimal tests ---

    #[test]
    fn add_bigint() {
        let f = AddFunc;
        let a = Value::BigInt(BigInt::from(1_000_000_000_000i64));
        let b = Value::BigInt(BigInt::from(2_000_000_000_000i64));
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(3_000_000_000_000i64)));
    }

    #[test]
    fn add_bigint_and_int64() {
        let f = AddFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::Int64(50);
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(150)));
    }

    #[test]
    fn add_decimal() {
        let f = AddFunc;
        let a = Value::Decimal("1.5".parse().unwrap());
        let b = Value::Decimal("2.5".parse().unwrap());
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::Decimal("4.0".parse().unwrap()));
    }

    #[test]
    fn subtract_bigint() {
        let f = SubtractFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(30));
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(70)));
    }

    #[test]
    fn multiply_bigint() {
        let f = MultiplyFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(200));
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(20_000)));
    }

    #[test]
    fn divide_bigint() {
        let f = DivideFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(4));
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(25)));
    }

    #[test]
    fn divide_bigint_by_zero() {
        let f = DivideFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(0));
        let result = f.evaluate(vec![a, b], &ctx());
        assert!(matches!(result, Err(FuncError::DivisionByZero)));
    }

    #[test]
    fn modulo_bigint() {
        let f = ModuloFunc;
        let a = Value::BigInt(BigInt::from(17));
        let b = Value::BigInt(BigInt::from(5));
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(2)));
    }

    #[test]
    fn negate_bigint() {
        let f = NegateFunc;
        let a = Value::BigInt(BigInt::from(42));
        let result = f.evaluate(vec![a], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(-42)));
    }

    #[test]
    fn abs_bigint_negative() {
        let f = AbsFunc;
        let a = Value::BigInt(BigInt::from(-99));
        let result = f.evaluate(vec![a], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(99)));
    }

    #[test]
    fn abs_decimal_negative() {
        let f = AbsFunc;
        let a = Value::Decimal("-3.14".parse().unwrap());
        let result = f.evaluate(vec![a], &ctx()).unwrap();
        assert_eq!(result, Value::Decimal("3.14".parse().unwrap()));
    }

    #[test]
    fn sign_bigint_negative() {
        let f = SignFunc;
        let a = Value::BigInt(BigInt::from(-42));
        let result = f.evaluate(vec![a], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(-1));
    }

    #[test]
    fn sign_decimal_positive() {
        let f = SignFunc;
        let a = Value::Decimal("7.5".parse().unwrap());
        let result = f.evaluate(vec![a], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(1));
    }

    #[test]
    fn min_bigint() {
        let f = MinFunc;
        let a = Value::BigInt(BigInt::from(100));
        let b = Value::BigInt(BigInt::from(50));
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::BigInt(BigInt::from(50)));
    }

    #[test]
    fn max_decimal() {
        let f = MaxFunc;
        let a = Value::Decimal("1.5".parse().unwrap());
        let b = Value::Decimal("2.7".parse().unwrap());
        let result = f.evaluate(vec![a, b], &ctx()).unwrap();
        assert_eq!(result, Value::Decimal("2.7".parse().unwrap()));
    }

    // -----------------------------------------------------------------------
    // Property tests: crash-freedom and arithmetic bounds
    // -----------------------------------------------------------------------

    mod proptests {
        use super::*;
        use crate::registry::FunctionRegistry;
        use crate::signature::ExprFunction;
        use crate::test_support::ctx;
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
            func.evaluate(vec![a, b], &ctx())
        }

        fn eval_unary(func: &dyn ExprFunction, a: Value) -> Result<Value, FuncError> {
            func.evaluate(vec![a], &ctx())
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
            REGISTRY.resolve(name, 2).expect("function must exist")
        }

        fn cast_func(name: &str, arity: usize) -> &'static dyn ExprFunction {
            REGISTRY.resolve(name, arity).expect("function must exist")
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
                let _ = f.evaluate(
                    vec![a, Value::Int64(38), Value::Int64(10)],
                    &ctx(),
                );
            }
        }
    }
}
