use crate::convert::{ConversionContext, convert};
use crate::data_type::DataType;
use crate::value::Value;

/// Verify the evaluated Value matches the sink DataType; if not, attempt
/// value-aware narrowing (int/float fit check) or lossless widening via
/// [`convert`]. Errors when the value cannot be represented
/// in the target type.
pub fn ensure_sink_compatible(value: Value, sink_dt: &DataType) -> Result<Value, String> {
    if let Some(ref value_dt) = value.data_type() {
        if value_dt == sink_dt {
            return Ok(value);
        }
        // Text/Bytes: check actual length against the sink's declared size.
        match (&value, sink_dt) {
            (Value::Text(s), DataType::Text { size: Some(max) }) => {
                let chars = s.chars().count();
                if chars > *max as usize {
                    return Err(format!("text length {chars} exceeds sink size {max}"));
                }
                return Ok(value);
            }
            (Value::Bytes(b), DataType::Bytes { size: Some(max) }) => {
                if b.len() > *max as usize {
                    return Err(format!("bytes length {} exceeds sink size {max}", b.len()));
                }
                return Ok(value);
            }
            _ => {}
        }
        // Numeric narrowing: TOML gives us Int64/Float64, but the sink may
        // be a narrower type. Check the actual value fits, then cast.
        if let Some(narrowed) = try_narrow_numeric(&value, sink_dt) {
            return narrowed;
        }
        let ctx = ConversionContext::passthrough();
        return convert(value, value_dt, sink_dt, &ctx)
            .map_err(|e| format!("cannot convert {value_dt} to {sink_dt}: {e}"));
    }
    Ok(value)
}

/// Try to narrow a numeric Value to the target DataType by checking the
/// actual value fits. Returns `None` if this is not a numeric narrowing
/// case — the caller should fall through to `convert()`.
fn try_narrow_numeric(value: &Value, target: &DataType) -> Option<Result<Value, String>> {
    let n = match value {
        Value::Int64(n) => *n,
        Value::Float64(f) => {
            return match target {
                DataType::Float32 => Some(Ok(Value::Float32(*f as f32))),
                _ => None,
            };
        }
        _ => return None,
    };

    let result = match target {
        DataType::Int8 if (i8::MIN as i64..=i8::MAX as i64).contains(&n) => Value::Int8(n as i8),
        DataType::Int16 if (i16::MIN as i64..=i16::MAX as i64).contains(&n) => {
            Value::Int16(n as i16)
        }
        DataType::Int32 if (i32::MIN as i64..=i32::MAX as i64).contains(&n) => {
            Value::Int32(n as i32)
        }
        DataType::UInt8 if (0..=u8::MAX as i64).contains(&n) => Value::UInt8(n as u8),
        DataType::UInt16 if (0..=u16::MAX as i64).contains(&n) => Value::UInt16(n as u16),
        DataType::UInt32 if (0..=u32::MAX as i64).contains(&n) => Value::UInt32(n as u32),
        DataType::UInt64 if n >= 0 => Value::UInt64(n as u64),
        DataType::Float32 => Value::Float32(n as f32),
        DataType::Float64 => Value::Float64(n as f64),
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => {
            return Some(Err(format!("value {n} out of range for {target}")));
        }
        _ => return None,
    };
    Some(Ok(result))
}
