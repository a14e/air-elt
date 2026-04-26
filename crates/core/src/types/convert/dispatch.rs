//! `(src, dst)` dispatch for value conversion. See module docs.

use super::ConvertError;
use super::uuid;
use crate::types::{DataType, Value};

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
}
