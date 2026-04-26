//! `(src, dst)` dispatch for value conversion. See module docs.

use super::ConvertError;
use super::uuid;
use crate::types::{DataType, Value};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;

/// Convert a single `Value` from `src` to `dst`.
pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }

    // Identity by exact equality (sized text/bytes shrinks to identity when
    // sizes match exactly). Pure widening (e.g. text(10) → text(20)) keeps
    // the same `Value::Text` payload — no transformation needed.
    if src == dst {
        return Ok(value);
    }
    if matches!(
        (src, dst),
        (DataType::Text { .. }, DataType::Text { .. })
            | (DataType::Bytes { .. }, DataType::Bytes { .. })
            | (DataType::BigInt { .. }, DataType::BigInt { .. })
            | (DataType::Decimal { .. }, DataType::Decimal { .. })
    ) {
        return Ok(value);
    }

    match (src, dst) {
        (DataType::Uuid, DataType::Text { .. }) => match value {
            Value::Uuid(u) => Ok(Value::Text(uuid::to_text(u))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Uuid, DataType::Bytes { .. }) => match value {
            Value::Uuid(u) => Ok(Value::Bytes(uuid::to_bytes(u).to_vec())),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Text { .. }, DataType::Uuid) => match value {
            Value::Text(s) => Ok(Value::Uuid(uuid::parse_text(&s)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bytes { .. }, DataType::Uuid) => match value {
            Value::Bytes(b) => Ok(Value::Uuid(uuid::from_bytes(&b)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        (DataType::Int16 | DataType::Int32 | DataType::Int64, DataType::Bool) => {
            let n: i64 = match value {
                Value::Int16(n) => n as i64,
                Value::Int32(n) => n as i64,
                Value::Int64(n) => n,
                _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
            };
            Ok(Value::Bool(n != 0))
        }
        (
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64,
            DataType::Bool,
        ) => {
            let n: u64 = match value {
                Value::UInt8(n) => n as u64,
                Value::UInt16(n) => n as u64,
                Value::UInt32(n) => n as u64,
                Value::UInt64(n) => n,
                _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
            };
            Ok(Value::Bool(n != 0))
        }
        (DataType::Bool, DataType::UInt8) => match value {
            Value::Bool(b) => Ok(Value::UInt8(b as u8)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bool, DataType::UInt16) => match value {
            Value::Bool(b) => Ok(Value::UInt16(b as u16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bool, DataType::UInt32) => match value {
            Value::Bool(b) => Ok(Value::UInt32(b as u32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bool, DataType::UInt64) => match value {
            Value::Bool(b) => Ok(Value::UInt64(b as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        (DataType::Bool, DataType::Int16) => match value {
            Value::Bool(b) => Ok(Value::Int16(b as i16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bool, DataType::Int32) => match value {
            Value::Bool(b) => Ok(Value::Int32(b as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bool, DataType::Int64) => match value {
            Value::Bool(b) => Ok(Value::Int64(b as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Numeric widening: rewrite the Value variant to match the sink
        // type so the sink codec binds the correct wire type. Relying on
        // driver-level coercion is not portable across pg/mysql binary
        // protocols.
        (DataType::Int16, DataType::Int32) => match value {
            Value::Int16(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int16, DataType::Int64) => match value {
            Value::Int16(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int32, DataType::Int64) => match value {
            Value::Int32(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int16, DataType::Float32) => match value {
            Value::Int16(n) => Ok(Value::Float32(n as f32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int16, DataType::Float64) => match value {
            Value::Int16(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int32, DataType::Float64) => match value {
            Value::Int32(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Float32, DataType::Float64) => match value {
            Value::Float32(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Fixed-width int → BigInt: trivial, no decimal arithmetic involved.
        (DataType::Int16, DataType::BigInt { .. }) => match value {
            Value::Int16(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int32, DataType::BigInt { .. }) => match value {
            Value::Int32(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int64, DataType::BigInt { .. }) => match value {
            Value::Int64(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Fixed-width int → Decimal: BigDecimal::from(int) is a metadata-only
        // wrap (no division / scaling).
        (DataType::Int16, DataType::Decimal { .. }) => match value {
            Value::Int16(n) => Ok(Value::Decimal(BigDecimal::from(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int32, DataType::Decimal { .. }) => match value {
            Value::Int32(n) => Ok(Value::Decimal(BigDecimal::from(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Int64, DataType::Decimal { .. }) => match value {
            Value::Int64(n) => Ok(Value::Decimal(BigDecimal::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // BigInt → Decimal: lift via BigDecimal::new(bigint, 0). No
        // multiplication, just packaging.
        (DataType::BigInt { .. }, DataType::Decimal { .. }) => match value {
            Value::BigInt(b) => Ok(Value::Decimal(BigDecimal::new(b, 0))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned widening within unsigned: rewrite the variant.
        (DataType::UInt8, DataType::UInt16) => match value {
            Value::UInt8(n) => Ok(Value::UInt16(n as u16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt8, DataType::UInt32) => match value {
            Value::UInt8(n) => Ok(Value::UInt32(n as u32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt8, DataType::UInt64) => match value {
            Value::UInt8(n) => Ok(Value::UInt64(n as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt16, DataType::UInt32) => match value {
            Value::UInt16(n) => Ok(Value::UInt32(n as u32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt16, DataType::UInt64) => match value {
            Value::UInt16(n) => Ok(Value::UInt64(n as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt32, DataType::UInt64) => match value {
            Value::UInt32(n) => Ok(Value::UInt64(n as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → signed (matrix already enforces width fits).
        (DataType::UInt8, DataType::Int16) => match value {
            Value::UInt8(n) => Ok(Value::Int16(n as i16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt8, DataType::Int32) => match value {
            Value::UInt8(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt8, DataType::Int64) => match value {
            Value::UInt8(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt16, DataType::Int32) => match value {
            Value::UInt16(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt16, DataType::Int64) => match value {
            Value::UInt16(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt32, DataType::Int64) => match value {
            Value::UInt32(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → BigInt.
        (DataType::UInt8, DataType::BigInt { .. }) => match value {
            Value::UInt8(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt16, DataType::BigInt { .. }) => match value {
            Value::UInt16(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt32, DataType::BigInt { .. }) => match value {
            Value::UInt32(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt64, DataType::BigInt { .. }) => match value {
            Value::UInt64(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → Decimal: BigDecimal::from accepts u64.
        (DataType::UInt8, DataType::Decimal { .. }) => match value {
            Value::UInt8(n) => Ok(Value::Decimal(BigDecimal::from(n as u64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt16, DataType::Decimal { .. }) => match value {
            Value::UInt16(n) => Ok(Value::Decimal(BigDecimal::from(n as u64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt32, DataType::Decimal { .. }) => match value {
            Value::UInt32(n) => Ok(Value::Decimal(BigDecimal::from(n as u64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::UInt64, DataType::Decimal { .. }) => match value {
            Value::UInt64(n) => Ok(Value::Decimal(BigDecimal::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        _ => Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ::uuid::Uuid as UuidVal;

    fn dt_text(n: u32) -> DataType {
        DataType::Text { size: Some(n) }
    }
    fn dt_bytes(n: u32) -> DataType {
        DataType::Bytes { size: Some(n) }
    }

    #[test]
    fn null_passthrough() {
        let out = convert(Value::Null, &DataType::Uuid, &dt_text(36)).unwrap();
        assert!(matches!(out, Value::Null));
    }

    #[test]
    fn identity_passthrough() {
        let out = convert(Value::Int32(7), &DataType::Int32, &DataType::Int32).unwrap();
        assert_eq!(out, Value::Int32(7));
    }

    #[test]
    fn text_widening_unchanged() {
        let out = convert(Value::Text("hi".into()), &dt_text(2), &dt_text(10)).unwrap();
        assert_eq!(out, Value::Text("hi".into()));
    }

    #[test]
    fn uuid_to_text_canonical() {
        let u = UuidVal::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let out = convert(Value::Uuid(u), &DataType::Uuid, &dt_text(36)).unwrap();
        assert_eq!(
            out,
            Value::Text("550e8400-e29b-41d4-a716-446655440000".into())
        );
    }

    #[test]
    fn uuid_to_bytes_16() {
        let u = UuidVal::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let out = convert(Value::Uuid(u), &DataType::Uuid, &dt_bytes(16)).unwrap();
        let Value::Bytes(b) = out else {
            panic!("expected bytes");
        };
        assert_eq!(b.len(), 16);
    }

    #[test]
    fn text_to_uuid_accepts_all_three_formats() {
        let canonical = "550e8400-e29b-41d4-a716-446655440000";
        let no_dash = "550e8400e29b41d4a716446655440000";
        let braced = "{550e8400-e29b-41d4-a716-446655440000}";
        for input in [canonical, no_dash, braced] {
            let out = convert(Value::Text(input.into()), &dt_text(38), &DataType::Uuid).unwrap();
            let Value::Uuid(u) = out else {
                panic!("expected uuid");
            };
            assert_eq!(u.to_string(), canonical);
        }
    }

    #[test]
    fn text_to_uuid_invalid_input_errors() {
        let res = convert(Value::Text("garbage".into()), &dt_text(36), &DataType::Uuid);
        assert!(res.is_err());
    }

    #[test]
    fn bytes_to_uuid_wrong_length_errors() {
        let res = convert(Value::Bytes(vec![0; 8]), &dt_bytes(8), &DataType::Uuid);
        assert!(matches!(res, Err(ConvertError::Length { .. })));
    }

    #[test]
    fn int_to_bool_rules() {
        assert_eq!(
            convert(Value::Int32(0), &DataType::Int32, &DataType::Bool).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            convert(Value::Int32(7), &DataType::Int32, &DataType::Bool).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            convert(Value::Int64(-1), &DataType::Int64, &DataType::Bool).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            convert(Value::Int16(0), &DataType::Int16, &DataType::Bool).unwrap(),
            Value::Bool(false)
        );
    }

    #[test]
    fn bool_to_int_rules() {
        assert_eq!(
            convert(Value::Bool(true), &DataType::Bool, &DataType::Int16).unwrap(),
            Value::Int16(1)
        );
        assert_eq!(
            convert(Value::Bool(false), &DataType::Bool, &DataType::Int64).unwrap(),
            Value::Int64(0)
        );
    }

    #[test]
    fn numeric_widening_rewrites_variant() {
        assert_eq!(
            convert(Value::Int16(7), &DataType::Int16, &DataType::Int32).unwrap(),
            Value::Int32(7)
        );
        assert_eq!(
            convert(Value::Int32(7), &DataType::Int32, &DataType::Int64).unwrap(),
            Value::Int64(7)
        );
        assert_eq!(
            convert(Value::Int16(7), &DataType::Int16, &DataType::Float64).unwrap(),
            Value::Float64(7.0)
        );
        assert_eq!(
            convert(Value::Int32(-3), &DataType::Int32, &DataType::Float64).unwrap(),
            Value::Float64(-3.0)
        );
        assert_eq!(
            convert(Value::Float32(1.5), &DataType::Float32, &DataType::Float64).unwrap(),
            Value::Float64(1.5)
        );
    }

    #[test]
    fn unsupported_pair_errors() {
        let res = convert(Value::Text("x".into()), &dt_text(1), &DataType::Json);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn null_passthrough_for_int_to_bool() {
        let out = convert(Value::Null, &DataType::Int32, &DataType::Bool).unwrap();
        assert_eq!(out, Value::Null);
    }

    fn dt_dec(p: u32, s: u32) -> DataType {
        DataType::Decimal {
            precision: Some(p),
            scale: Some(s),
        }
    }
    const DT_BIGINT: DataType = DataType::BigInt { width: None };

    #[test]
    fn int_to_bigint_wraps_value() {
        let out = convert(Value::Int32(42), &DataType::Int32, &DT_BIGINT).unwrap();
        assert_eq!(out, Value::BigInt(BigInt::from(42)));
        let out = convert(Value::Int64(-7), &DataType::Int64, &DT_BIGINT).unwrap();
        assert_eq!(out, Value::BigInt(BigInt::from(-7)));
    }

    #[test]
    fn int_to_decimal_wraps_value() {
        let out = convert(Value::Int32(42), &DataType::Int32, &dt_dec(10, 0)).unwrap();
        assert_eq!(out, Value::Decimal(BigDecimal::from(42)));
    }

    #[test]
    fn bigint_to_decimal_wraps_value() {
        let big = BigInt::from(1_000_000_i64);
        let out = convert(Value::BigInt(big.clone()), &DT_BIGINT, &dt_dec(10, 0)).unwrap();
        assert_eq!(out, Value::Decimal(BigDecimal::new(big, 0)));
    }

    #[test]
    fn bigint_to_bigint_widening_passthrough() {
        let big = BigInt::from(1_234_567_890_i64);
        let out = convert(
            Value::BigInt(big.clone()),
            &DataType::BigInt { width: Some(10) },
            &DT_BIGINT,
        )
        .unwrap();
        assert_eq!(out, Value::BigInt(big));
    }

    #[test]
    fn unsigned_to_unsigned_widens() {
        assert_eq!(
            convert(Value::UInt8(200), &DataType::UInt8, &DataType::UInt32).unwrap(),
            Value::UInt32(200)
        );
        assert_eq!(
            convert(
                Value::UInt32(u32::MAX),
                &DataType::UInt32,
                &DataType::UInt64
            )
            .unwrap(),
            Value::UInt64(u32::MAX as u64)
        );
    }

    #[test]
    fn unsigned_to_signed_widens() {
        assert_eq!(
            convert(Value::UInt8(255), &DataType::UInt8, &DataType::Int16).unwrap(),
            Value::Int16(255)
        );
        assert_eq!(
            convert(Value::UInt32(u32::MAX), &DataType::UInt32, &DataType::Int64).unwrap(),
            Value::Int64(u32::MAX as i64)
        );
    }

    #[test]
    fn unsigned_to_bigint_and_decimal() {
        assert_eq!(
            convert(Value::UInt64(u64::MAX), &DataType::UInt64, &DT_BIGINT).unwrap(),
            Value::BigInt(BigInt::from(u64::MAX))
        );
        assert_eq!(
            convert(Value::UInt8(7), &DataType::UInt8, &dt_dec(10, 0)).unwrap(),
            Value::Decimal(BigDecimal::from(7u64))
        );
    }

    #[test]
    fn unsigned_to_bool_rules() {
        assert_eq!(
            convert(Value::UInt8(0), &DataType::UInt8, &DataType::Bool).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            convert(Value::UInt16(2), &DataType::UInt16, &DataType::Bool).unwrap(),
            Value::Bool(true)
        );
        assert_eq!(
            convert(Value::UInt32(0), &DataType::UInt32, &DataType::Bool).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            convert(Value::UInt64(1), &DataType::UInt64, &DataType::Bool).unwrap(),
            Value::Bool(true)
        );
    }

    #[test]
    fn bool_to_unsigned_full_matrix() {
        for (dst, expected) in [
            (DataType::UInt8, Value::UInt8(1)),
            (DataType::UInt16, Value::UInt16(1)),
            (DataType::UInt32, Value::UInt32(1)),
            (DataType::UInt64, Value::UInt64(1)),
        ] {
            assert_eq!(
                convert(Value::Bool(true), &DataType::Bool, &dst).unwrap(),
                expected
            );
        }
        for (dst, expected) in [
            (DataType::UInt8, Value::UInt8(0)),
            (DataType::UInt16, Value::UInt16(0)),
            (DataType::UInt32, Value::UInt32(0)),
            (DataType::UInt64, Value::UInt64(0)),
        ] {
            assert_eq!(
                convert(Value::Bool(false), &DataType::Bool, &dst).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn unsigned_widening_full_matrix() {
        // Every UInt → wider UInt arm must rewrite the variant.
        let cases = [
            (
                Value::UInt8(7),
                DataType::UInt8,
                DataType::UInt16,
                Value::UInt16(7),
            ),
            (
                Value::UInt8(7),
                DataType::UInt8,
                DataType::UInt64,
                Value::UInt64(7),
            ),
            (
                Value::UInt16(70),
                DataType::UInt16,
                DataType::UInt32,
                Value::UInt32(70),
            ),
            (
                Value::UInt16(70),
                DataType::UInt16,
                DataType::UInt64,
                Value::UInt64(70),
            ),
            (
                Value::UInt32(700),
                DataType::UInt32,
                DataType::UInt64,
                Value::UInt64(700),
            ),
        ];
        for (val, src, dst, expected) in cases {
            assert_eq!(convert(val, &src, &dst).unwrap(), expected);
        }
    }

    #[test]
    fn unsigned_to_signed_full_matrix() {
        let cases = [
            (
                Value::UInt8(255),
                DataType::UInt8,
                DataType::Int32,
                Value::Int32(255),
            ),
            (
                Value::UInt8(255),
                DataType::UInt8,
                DataType::Int64,
                Value::Int64(255),
            ),
            (
                Value::UInt16(65_535),
                DataType::UInt16,
                DataType::Int64,
                Value::Int64(65_535),
            ),
        ];
        for (val, src, dst, expected) in cases {
            assert_eq!(convert(val, &src, &dst).unwrap(), expected);
        }
    }

    #[test]
    fn unsigned_to_bigint_full_matrix() {
        let cases = [
            (Value::UInt8(7), DataType::UInt8, BigInt::from(7)),
            (Value::UInt16(70), DataType::UInt16, BigInt::from(70)),
            (
                Value::UInt32(70_000),
                DataType::UInt32,
                BigInt::from(70_000),
            ),
            (
                Value::UInt64(u64::MAX),
                DataType::UInt64,
                BigInt::from(u64::MAX),
            ),
        ];
        for (val, src, expected) in cases {
            assert_eq!(
                convert(val, &src, &DT_BIGINT).unwrap(),
                Value::BigInt(expected)
            );
        }
    }

    #[test]
    fn value_shape_mismatch_errors_on_wrong_payload() {
        // src=UInt8 but the value carries an Int32 — guards against silent
        // mis-binds if the runner ever feeds the wrong variant.
        let res = convert(Value::Int32(1), &DataType::UInt8, &DataType::UInt16);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn unsupported_reverse_paths_error() {
        // Sanity-check that `convert` itself refuses lossy paths the matrix
        // already filters at validation time. Belt-and-braces.
        for (src, dst) in [
            (DataType::Int16, DataType::UInt16),
            (DT_BIGINT, DataType::UInt64),
            (dt_dec(10, 2), DataType::UInt32),
            (DataType::Float64, DataType::UInt64),
        ] {
            // Use a placeholder value of the source variant so we hit the
            // dispatch fall-through (Unsupported), not ValueShapeMismatch.
            let placeholder = match src {
                DataType::Int16 => Value::Int16(1),
                DataType::BigInt { .. } => Value::BigInt(BigInt::from(1)),
                DataType::Decimal { .. } => Value::Decimal(BigDecimal::from(1)),
                DataType::Float64 => Value::Float64(1.0),
                _ => unreachable!(),
            };
            let res = convert(placeholder, &src, &dst);
            assert!(
                matches!(res, Err(ConvertError::Unsupported { .. })),
                "{src:?} → {dst:?} expected Unsupported, got {res:?}"
            );
        }
    }

    #[test]
    fn decimal_to_decimal_widening_passthrough() {
        let d: BigDecimal = "12.34".parse().unwrap();
        let out = convert(Value::Decimal(d.clone()), &dt_dec(4, 2), &dt_dec(10, 4)).unwrap();
        assert_eq!(out, Value::Decimal(d));
    }
}
