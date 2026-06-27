use air_elt_expr_types::error::ExprTypeError;
use air_elt_expr_types::limits::MAX_BIGINT_WIDTH;
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use num_traits::ToPrimitive;

use crate::error::FuncError;

/// Convert any numeric [`Value`] to `f64` for the float-domain math builtins
/// (trig, logarithms, `sqrt`, `power`, `isNaN`, `isInfinite`). Covers EVERY
/// numeric variant — narrow and unsigned integers, `BigInt`, and `Decimal` — so a
/// source column arriving as `Int32` / `UInt32` / `BigInt` is accepted rather than
/// rejected at runtime (the arithmetic builtins canonicalise via `widen_numeric`;
/// the float-domain ones go straight through `f64`). A `BigInt` / `Decimal` past
/// the `f64` range converts to `±INFINITY` — the IEEE result that `to_f64()`
/// returns as `Some(inf)`, which `isInfinite` then reports faithfully. On the rare
/// path where `num-traits` returns `None` (no finite or infinite representation),
/// and for any non-numeric value, this is a `TypeMismatch`.
pub(crate) fn value_to_f64(val: &Value, func_name: &str) -> Result<f64, FuncError> {
    let mismatch = || FuncError::TypeMismatch {
        function: func_name.to_owned(),
        expected: "numeric".to_owned(),
        actual: format!("{:?}", val.data_type()),
    };
    match val {
        Value::Int8(x) => Ok(f64::from(*x)),
        Value::Int16(x) => Ok(f64::from(*x)),
        Value::Int32(x) => Ok(f64::from(*x)),
        Value::Int64(x) => Ok(*x as f64),
        Value::UInt8(x) => Ok(f64::from(*x)),
        Value::UInt16(x) => Ok(f64::from(*x)),
        Value::UInt32(x) => Ok(f64::from(*x)),
        Value::UInt64(x) => Ok(*x as f64),
        Value::Float32(x) => Ok(f64::from(*x)),
        Value::Float64(x) => Ok(*x),
        Value::BigInt(x) => x.to_f64().ok_or_else(mismatch),
        Value::Decimal(x) => x.to_f64().ok_or_else(mismatch),
        _ => Err(mismatch()),
    }
}

/// Compute the result type of arithmetic between two expression types.
/// When both operands carry `int_bound`, uses precise bit arithmetic.
/// Otherwise falls back to DataType-level bounds.
pub fn arithmetic_result_type(
    op: ArithmeticOp,
    left: &NullableExprType,
    right: &NullableExprType,
) -> Result<NullableExprType, ExprTypeError> {
    let nullable = left.nullable || right.nullable;

    // If both have int_bound, use precise bit arithmetic
    if let (Some(l_bits), Some(r_bits)) = (left.int_bound, right.int_bound) {
        let result_bits: u16 = match op {
            ArithmeticOp::Add | ArithmeticOp::Subtract => (l_bits.max(r_bits) as u16) + 1,
            ArithmeticOp::Multiply => (l_bits as u16) + (r_bits as u16),
            ArithmeticOp::Divide => l_bits as u16,
            ArithmeticOp::Modulo => l_bits.min(r_bits) as u16,
        };

        if result_bits > 64 {
            let decimal_digits =
                ((result_bits as f64) * std::f64::consts::LOG10_2).ceil() as u32 + 1;
            if decimal_digits > MAX_BIGINT_WIDTH {
                return Err(ExprTypeError::IntegerOverflow {
                    max: MAX_BIGINT_WIDTH,
                });
            }
            return Ok(NullableExprType {
                data_type: DataType::BigInt {
                    width: Some(decimal_digits),
                },
                nullable,
                int_bound: None,
            });
        }

        return Ok(NullableExprType {
            data_type: DataType::Int64,
            nullable,
            int_bound: Some(result_bits as u8),
        });
    }

    // Fallback: use DataType-level bounds (no precise int_bound available)
    let result_dt = scalar_arithmetic(op, &left.data_type, &right.data_type)?;
    Ok(NullableExprType {
        data_type: result_dt,
        nullable,
        int_bound: None,
    })
}

/// Result type of `min` / `max` over `args`: the single common type every
/// argument coerces to. Numeric arguments widen to one common numeric type
/// (a plain widening join — no bit inflation, unlike arithmetic); identical
/// non-numeric comparable categories pass through (`Text`/`Bytes` widen to
/// unbounded). Mixing categories whose ordering is undefined against each
/// other (e.g. a number and text) is rejected — `min`/`max` are not defined
/// on such a comparison.
///
/// `min`/`max` skip NULL arguments and yield NULL only when EVERY argument is
/// NULL, so the result is nullable exactly when every argument is nullable: a
/// single non-null argument guarantees a non-null extremum.
pub fn comparable_join(
    args: &[NullableExprType],
    op: &str,
) -> Result<NullableExprType, ExprTypeError> {
    let mut accumulator = args[0].data_type.clone();
    for arg in &args[1..] {
        accumulator = join_comparable(&accumulator, &arg.data_type, op)?;
    }
    let nullable = args.iter().all(|arg| arg.nullable);
    Ok(NullableExprType::new(accumulator, nullable))
}

fn join_comparable(left: &DataType, right: &DataType, op: &str) -> Result<DataType, ExprTypeError> {
    if left == right {
        return Ok(left.clone());
    }
    if is_numeric(left) && is_numeric(right) {
        return numeric_join(left, right, op);
    }
    match (left, right) {
        (DataType::Text { .. }, DataType::Text { .. }) => Ok(DataType::text()),
        (DataType::Bytes { .. }, DataType::Bytes { .. }) => Ok(DataType::bytes()),
        _ => Err(type_mismatch(op, left, right)),
    }
}

/// Widening join of two numeric types (no bit inflation): the narrower type
/// widens into the wider one. Mirrors the type pairs of [`scalar_arithmetic`]
/// but keeps the width rather than growing it.
fn numeric_join(left: &DataType, right: &DataType, op: &str) -> Result<DataType, ExprTypeError> {
    use DataType::*;
    match (left, right) {
        (l, r) if is_integer(l) && is_integer(r) => {
            bits_to_int_type(integer_bits(l).max(integer_bits(r)))
        }
        (Float32, Float32) => Ok(Float32),
        (l, r) if is_float(l) && is_float(r) => Ok(Float64),
        (l, r) if (is_integer(l) && is_float(r)) || (is_float(l) && is_integer(r)) => Ok(Float64),
        (BigInt { .. }, BigInt { .. }) => Ok(BigInt { width: None }),
        (l, BigInt { .. }) | (BigInt { .. }, l) if is_integer(l) => Ok(BigInt { width: None }),
        (l, r)
            if (is_numeric(l) && matches!(r, Decimal { .. }))
                || (matches!(l, Decimal { .. }) && is_numeric(r)) =>
        {
            Ok(Decimal {
                precision: None,
                scale: None,
            })
        }
        _ => Err(type_mismatch(op, left, right)),
    }
}

fn type_mismatch(op: &str, left: &DataType, right: &DataType) -> ExprTypeError {
    ExprTypeError::TypeMismatch {
        operation: op.to_owned(),
        left: format!("{left}"),
        right: format!("{right}"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithmeticOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Modulo,
}

impl ArithmeticOp {
    pub fn name(self) -> &'static str {
        match self {
            Self::Add => "add",
            Self::Subtract => "subtract",
            Self::Multiply => "multiply",
            Self::Divide => "divide",
            Self::Modulo => "modulo",
        }
    }
}

/// Compute result type for string concatenation.
pub fn concat_result_type(types: &[&DataType]) -> Result<DataType, ExprTypeError> {
    let mut total_size: Option<u32> = Some(0);
    for t in types {
        match t {
            DataType::Text { size: Some(s) } => {
                total_size = total_size.map(|acc| acc.saturating_add(*s));
            }
            DataType::Text { size: None } => {
                total_size = None;
            }
            _ => {
                return Err(ExprTypeError::TypeMismatch {
                    operation: "concat".to_owned(),
                    left: format!("{t}"),
                    right: "Text".to_owned(),
                });
            }
        }
    }
    Ok(DataType::Text { size: total_size })
}

/// Unify two array element types for `add`-array / `concat`-array, mirroring
/// the spirit of [`concat_result_type`] for elements rather than text sizes.
///
/// * a `None` side (the empty/unknown element of `[]`) yields the other side;
/// * identical element types collapse to themselves;
/// * otherwise the two must be mutually [`is_compatible`](air_elt_types::matrix::is_compatible)
///   (numeric widening, UUID↔text/bytes, …), in which case the result is the
///   wider source type taken via the matrix; an incompatible pair returns
///   `Err`, which the caller turns into a `TypeMismatch`.
pub fn array_element_join(
    left: &Option<Box<DataType>>,
    right: &Option<Box<DataType>>,
) -> Result<Option<DataType>, ExprTypeError> {
    match (left, right) {
        (None, None) => Ok(None),
        (Some(l), None) => Ok(Some((**l).clone())),
        (None, Some(r)) => Ok(Some((**r).clone())),
        (Some(l), Some(r)) => {
            if l == r {
                return Ok(Some((**l).clone()));
            }
            let left_into_right =
                air_elt_types::matrix::is_compatible((**l).clone(), (**r).clone());
            let right_into_left =
                air_elt_types::matrix::is_compatible((**r).clone(), (**l).clone());
            if left_into_right {
                Ok(Some((**r).clone()))
            } else if right_into_left {
                Ok(Some((**l).clone()))
            } else {
                Err(ExprTypeError::TypeMismatch {
                    operation: "array concat".to_owned(),
                    left: format!("{l}"),
                    right: format!("{r}"),
                })
            }
        }
    }
}

fn scalar_arithmetic(
    op: ArithmeticOp,
    left: &DataType,
    right: &DataType,
) -> Result<DataType, ExprTypeError> {
    use DataType::*;

    match (left, right) {
        // Int + Int -> wider int (potentially BigInt)
        (l, r) if is_integer(l) && is_integer(r) => {
            let l_bits = integer_bits(l);
            let r_bits = integer_bits(r);
            let result_bits = match op {
                ArithmeticOp::Add | ArithmeticOp::Subtract => l_bits.max(r_bits) + 1,
                ArithmeticOp::Multiply => l_bits + r_bits,
                ArithmeticOp::Divide => l_bits,
                ArithmeticOp::Modulo => l_bits.min(r_bits),
            };
            bits_to_int_type(result_bits)
        }
        // Float + Float -> wider float
        (Float32, Float32) => Ok(Float32),
        (Float32, Float64) | (Float64, Float32) | (Float64, Float64) => Ok(Float64),
        // Int + Float -> Float64
        (l, r) if (is_integer(l) && is_float(r)) || (is_float(l) && is_integer(r)) => Ok(Float64),
        // BigInt + BigInt -> BigInt with wider width
        (BigInt { width: w1 }, BigInt { width: w2 }) => {
            let w1 = w1.unwrap_or(MAX_BIGINT_WIDTH);
            let w2 = w2.unwrap_or(MAX_BIGINT_WIDTH);
            let result_width = match op {
                ArithmeticOp::Add | ArithmeticOp::Subtract => w1.max(w2).saturating_add(1),
                ArithmeticOp::Multiply => w1.saturating_add(w2),
                ArithmeticOp::Divide => w1,
                ArithmeticOp::Modulo => w1.min(w2),
            };
            let capped = result_width.min(MAX_BIGINT_WIDTH);
            Ok(BigInt {
                width: Some(capped),
            })
        }
        // Int + BigInt -> BigInt
        (l, BigInt { width }) if is_integer(l) => {
            let l_width = bits_to_decimal_digits(integer_bits(l));
            let w = width.unwrap_or(MAX_BIGINT_WIDTH);
            let result = l_width.max(w).saturating_add(1).min(MAX_BIGINT_WIDTH);
            Ok(BigInt {
                width: Some(result),
            })
        }
        (BigInt { width }, r) if is_integer(r) => {
            let r_width = bits_to_decimal_digits(integer_bits(r));
            let w = width.unwrap_or(MAX_BIGINT_WIDTH);
            let result = r_width.max(w).saturating_add(1).min(MAX_BIGINT_WIDTH);
            Ok(BigInt {
                width: Some(result),
            })
        }
        // Decimal arithmetic
        (Decimal { .. }, Decimal { .. }) => Ok(Decimal {
            precision: None,
            scale: None,
        }),
        (l, r)
            if (is_numeric(l) && matches!(r, Decimal { .. }))
                || (matches!(l, Decimal { .. }) && is_numeric(r)) =>
        {
            Ok(Decimal {
                precision: None,
                scale: None,
            })
        }
        _ => Err(ExprTypeError::TypeMismatch {
            operation: op.name().to_owned(),
            left: format!("{left}"),
            right: format!("{right}"),
        }),
    }
}

fn is_integer(dt: &DataType) -> bool {
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
    )
}

fn is_float(dt: &DataType) -> bool {
    matches!(dt, DataType::Float32 | DataType::Float64)
}

fn is_numeric(dt: &DataType) -> bool {
    is_integer(dt)
        || is_float(dt)
        || matches!(dt, DataType::BigInt { .. } | DataType::Decimal { .. })
}

fn integer_bits(dt: &DataType) -> u32 {
    match dt {
        DataType::Int8 | DataType::UInt8 => 8,
        DataType::Int16 | DataType::UInt16 => 16,
        DataType::Int32 | DataType::UInt32 => 32,
        DataType::Int64 | DataType::UInt64 => 64,
        _ => 64,
    }
}

/// Convert bit width to approximate decimal digit count.
fn bits_to_decimal_digits(bits: u32) -> u32 {
    (bits as f64 * std::f64::consts::LOG10_2).ceil() as u32 + 1
}

fn bits_to_int_type(bits: u32) -> Result<DataType, ExprTypeError> {
    if bits <= 8 {
        Ok(DataType::Int8)
    } else if bits <= 16 {
        Ok(DataType::Int16)
    } else if bits <= 32 {
        Ok(DataType::Int32)
    } else if bits <= 64 {
        Ok(DataType::Int64)
    } else {
        let decimal_digits = bits_to_decimal_digits(bits);
        if decimal_digits > MAX_BIGINT_WIDTH {
            Err(ExprTypeError::IntegerOverflow {
                max: MAX_BIGINT_WIDTH,
            })
        } else {
            Ok(DataType::BigInt {
                width: Some(decimal_digits),
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn int64_add_int64_widens() {
        let left = NullableExprType::non_null(DataType::Int64);
        let right = NullableExprType::non_null(DataType::Int64);
        let result = arithmetic_result_type(ArithmeticOp::Add, &left, &right).unwrap();
        assert!(matches!(result.data_type, DataType::BigInt { .. }));
    }

    #[test]
    fn int32_add_int32_widens_to_int64() {
        let left = NullableExprType::non_null(DataType::Int32);
        let right = NullableExprType::non_null(DataType::Int32);
        let result = arithmetic_result_type(ArithmeticOp::Add, &left, &right).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
    }

    #[test]
    fn int8_multiply_int8_stays_int16() {
        let left = NullableExprType::non_null(DataType::Int8);
        let right = NullableExprType::non_null(DataType::Int8);
        let result = arithmetic_result_type(ArithmeticOp::Multiply, &left, &right).unwrap();
        assert_eq!(result.data_type, DataType::Int16);
    }

    #[test]
    fn float32_add_int32_yields_float64() {
        let left = NullableExprType::non_null(DataType::Float32);
        let right = NullableExprType::non_null(DataType::Int32);
        let result = arithmetic_result_type(ArithmeticOp::Add, &left, &right).unwrap();
        assert_eq!(result.data_type, DataType::Float64);
    }

    #[test]
    fn text_concat_sizes_add() {
        let a = DataType::Text { size: Some(10) };
        let b = DataType::Text { size: Some(20) };
        let result = concat_result_type(&[&a, &b]).unwrap();
        assert_eq!(result, DataType::Text { size: Some(30) });
    }

    #[test]
    fn text_concat_unbounded() {
        let a = DataType::Text { size: Some(10) };
        let b = DataType::Text { size: None };
        let result = concat_result_type(&[&a, &b]).unwrap();
        assert_eq!(result, DataType::Text { size: None });
    }

    #[test]
    fn int_bound_add_stays_small() {
        // 1 + 1: each is 1 bit, result is 2 bits -> Int64 with bound 2
        let left = NullableExprType::int_with_bound(DataType::Int64, 1);
        let right = NullableExprType::int_with_bound(DataType::Int64, 1);
        let result = arithmetic_result_type(ArithmeticOp::Add, &left, &right).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert_eq!(result.int_bound, Some(2));
    }

    #[test]
    fn int_bound_multiply_accumulates() {
        // 6 bits * 6 bits = 12 bits -> still Int64
        let left = NullableExprType::int_with_bound(DataType::Int64, 6);
        let right = NullableExprType::int_with_bound(DataType::Int64, 6);
        let result = arithmetic_result_type(ArithmeticOp::Multiply, &left, &right).unwrap();
        assert_eq!(result.data_type, DataType::Int64);
        assert_eq!(result.int_bound, Some(12));
    }

    #[test]
    fn int_bound_overflow_to_bigint() {
        // 40 bits * 40 bits = 80 bits -> BigInt
        let left = NullableExprType::int_with_bound(DataType::Int64, 40);
        let right = NullableExprType::int_with_bound(DataType::Int64, 40);
        let result = arithmetic_result_type(ArithmeticOp::Multiply, &left, &right).unwrap();
        assert!(matches!(result.data_type, DataType::BigInt { .. }));
        assert_eq!(result.int_bound, None);
    }

    #[test]
    fn array_element_join_collapses_identical() {
        let l = Some(Box::new(DataType::Int64));
        let r = Some(Box::new(DataType::Int64));
        assert_eq!(array_element_join(&l, &r).unwrap(), Some(DataType::Int64));
    }

    #[test]
    fn array_element_join_none_side_yields_other() {
        let some = Some(Box::new(DataType::Text { size: None }));
        assert_eq!(
            array_element_join(&None, &some).unwrap(),
            Some(DataType::Text { size: None })
        );
        assert_eq!(
            array_element_join(&some, &None).unwrap(),
            Some(DataType::Text { size: None })
        );
        assert_eq!(array_element_join(&None, &None).unwrap(), None);
    }

    #[test]
    fn array_element_join_widens_numeric() {
        // Int32 widens into Int64 via the compatibility matrix.
        let l = Some(Box::new(DataType::Int32));
        let r = Some(Box::new(DataType::Int64));
        assert_eq!(array_element_join(&l, &r).unwrap(), Some(DataType::Int64));
    }

    #[test]
    fn array_element_join_incompatible_errors() {
        // Int64 and Date have no conversion arm in either direction.
        let l = Some(Box::new(DataType::Int64));
        let r = Some(Box::new(DataType::Date));
        assert!(matches!(
            array_element_join(&l, &r),
            Err(ExprTypeError::TypeMismatch { .. })
        ));
    }
}
