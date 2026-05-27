use air_elt_types::DataType;

use crate::error::ExprTypeError;
use crate::limits::MAX_BIGINT_WIDTH;
use crate::nullable::NullableExprType;

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
}
