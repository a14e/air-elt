//! Saturating integer narrowing under `truncate=true`.
//!
//! Covers signed → smaller signed, unsigned → smaller unsigned, signed →
//! unsigned (negative → 0), and unsigned → signed. All routes saturate to
//! the target's representable range — no wrapping, never panics. Identity
//! and pure widening are handled by the dispatcher's short-circuit.

use super::error::ConvertError;
use super::saturate::*;
use crate::types::{DataType, Value};

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;

    // Lift the source into the widest signed/unsigned canonical form, then
    // saturate down to the target. Only the (src, value) variants that
    // actually exist for that DataType are accepted.
    match (src, dst) {
        // Signed → smaller signed.
        (Int64, Int32) => match value {
            Value::Int64(n) => Ok(Value::Int32(sat_i64_to_i32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, Int16) => match value {
            Value::Int64(n) => Ok(Value::Int16(sat_i64_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, Int16) => match value {
            Value::Int32(n) => Ok(Value::Int16(sat_i32_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → smaller unsigned.
        (UInt64, UInt32) => match value {
            Value::UInt64(n) => Ok(Value::UInt32(sat_u64_to_u32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, UInt16) => match value {
            Value::UInt64(n) => Ok(Value::UInt16(sat_u64_to_u16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, UInt8) => match value {
            Value::UInt64(n) => Ok(Value::UInt8(sat_u64_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, UInt16) => match value {
            Value::UInt32(n) => Ok(Value::UInt16(sat_u32_to_u16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, UInt8) => match value {
            Value::UInt32(n) => Ok(Value::UInt8(sat_u32_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt16, UInt8) => match value {
            Value::UInt16(n) => Ok(Value::UInt8(sat_u16_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Signed → unsigned (negatives → 0, then saturate).
        (Int64, UInt64) => match value {
            Value::Int64(n) => Ok(Value::UInt64(sat_i64_to_u64(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, UInt32) => match value {
            Value::Int64(n) => Ok(Value::UInt32(sat_i64_to_u32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, UInt16) => match value {
            Value::Int64(n) => Ok(Value::UInt16(sat_i64_to_u16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int64, UInt8) => match value {
            Value::Int64(n) => Ok(Value::UInt8(sat_i64_to_u8(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt64) => match value {
            Value::Int32(n) => Ok(Value::UInt64(sat_i64_to_u64(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt32) => match value {
            Value::Int32(n) => Ok(Value::UInt32(sat_i64_to_u32(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt16) => match value {
            Value::Int32(n) => Ok(Value::UInt16(sat_i64_to_u16(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int32, UInt8) => match value {
            Value::Int32(n) => Ok(Value::UInt8(sat_i64_to_u8(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt64) => match value {
            Value::Int16(n) => Ok(Value::UInt64(sat_i64_to_u64(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt32) => match value {
            Value::Int16(n) => Ok(Value::UInt32(sat_i64_to_u32(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt16) => match value {
            Value::Int16(n) => Ok(Value::UInt16(sat_i64_to_u16(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (Int16, UInt8) => match value {
            Value::Int16(n) => Ok(Value::UInt8(sat_i64_to_u8(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned → signed (saturate at signed max).
        (UInt64, Int64) => match value {
            Value::UInt64(n) => Ok(Value::Int64(sat_u64_to_i64(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, Int32) => match value {
            Value::UInt64(n) => Ok(Value::Int32(sat_u64_to_i32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt64, Int16) => match value {
            Value::UInt64(n) => Ok(Value::Int16(sat_u64_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, Int32) => match value {
            Value::UInt32(n) => Ok(Value::Int32(sat_u32_to_i32(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt32, Int16) => match value {
            Value::UInt32(n) => Ok(Value::Int16(sat_u32_to_i16(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (UInt16, Int16) => match value {
            Value::UInt16(n) => Ok(Value::Int16(sat_u16_to_i16(n))),
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

    /// Macro generates two assertions per (src, dst) saturating-cast arm:
    /// one in-range value (proves the arm uses the right output variant)
    /// and one *out-of-range* value (proves the arm dispatches to the
    /// correct saturating primitive — a swapped arm would saturate to
    /// the wrong target's max/min and the assertion would catch it).
    macro_rules! pair_tests {
        (
            $name:ident,
            $src_dt:expr, $dst_dt:expr,
            $ok_in:expr, $ok_out:expr,
            $sat_in:expr, $sat_out:expr
        ) => {
            #[test]
            fn $name() {
                assert_eq!(convert($ok_in, &$src_dt, &$dst_dt).unwrap(), $ok_out);
                assert_eq!(
                    convert($sat_in, &$src_dt, &$dst_dt).unwrap(),
                    $sat_out,
                    "saturation case for {} → {}",
                    stringify!($src_dt),
                    stringify!($dst_dt)
                );
            }
        };
    }

    // Signed → smaller signed.
    pair_tests!(
        i64_to_i32_ok,
        DataType::Int64,
        DataType::Int32,
        Value::Int64(7),
        Value::Int32(7),
        Value::Int64(i32::MAX as i64 + 1),
        Value::Int32(i32::MAX)
    );
    pair_tests!(
        i64_to_i16_ok,
        DataType::Int64,
        DataType::Int16,
        Value::Int64(7),
        Value::Int16(7),
        Value::Int64(i16::MAX as i64 + 1),
        Value::Int16(i16::MAX)
    );
    pair_tests!(
        i32_to_i16_ok,
        DataType::Int32,
        DataType::Int16,
        Value::Int32(7),
        Value::Int16(7),
        Value::Int32(40_000),
        Value::Int16(i16::MAX)
    );

    // Unsigned → smaller unsigned.
    pair_tests!(
        u64_to_u32_ok,
        DataType::UInt64,
        DataType::UInt32,
        Value::UInt64(7),
        Value::UInt32(7),
        Value::UInt64(u32::MAX as u64 + 1),
        Value::UInt32(u32::MAX)
    );
    pair_tests!(
        u64_to_u16_ok,
        DataType::UInt64,
        DataType::UInt16,
        Value::UInt64(7),
        Value::UInt16(7),
        Value::UInt64(u16::MAX as u64 + 1),
        Value::UInt16(u16::MAX)
    );
    pair_tests!(
        u64_to_u8_ok,
        DataType::UInt64,
        DataType::UInt8,
        Value::UInt64(7),
        Value::UInt8(7),
        Value::UInt64(u8::MAX as u64 + 1),
        Value::UInt8(u8::MAX)
    );
    pair_tests!(
        u32_to_u16_ok,
        DataType::UInt32,
        DataType::UInt16,
        Value::UInt32(7),
        Value::UInt16(7),
        Value::UInt32(u16::MAX as u32 + 1),
        Value::UInt16(u16::MAX)
    );
    pair_tests!(
        u32_to_u8_ok,
        DataType::UInt32,
        DataType::UInt8,
        Value::UInt32(7),
        Value::UInt8(7),
        Value::UInt32(u8::MAX as u32 + 1),
        Value::UInt8(u8::MAX)
    );
    pair_tests!(
        u16_to_u8_ok,
        DataType::UInt16,
        DataType::UInt8,
        Value::UInt16(7),
        Value::UInt8(7),
        Value::UInt16(u8::MAX as u16 + 1),
        Value::UInt8(u8::MAX)
    );

    // Signed → unsigned: negatives clamp to 0, positives saturate at unsigned max.
    pair_tests!(
        i64_to_u64_ok,
        DataType::Int64,
        DataType::UInt64,
        Value::Int64(7),
        Value::UInt64(7),
        Value::Int64(-1),
        Value::UInt64(0)
    );
    pair_tests!(
        i64_to_u32_ok,
        DataType::Int64,
        DataType::UInt32,
        Value::Int64(7),
        Value::UInt32(7),
        Value::Int64(u32::MAX as i64 + 1),
        Value::UInt32(u32::MAX)
    );
    pair_tests!(
        i64_to_u16_ok,
        DataType::Int64,
        DataType::UInt16,
        Value::Int64(7),
        Value::UInt16(7),
        Value::Int64(-1),
        Value::UInt16(0)
    );
    pair_tests!(
        i64_to_u8_ok,
        DataType::Int64,
        DataType::UInt8,
        Value::Int64(7),
        Value::UInt8(7),
        Value::Int64(u8::MAX as i64 + 1),
        Value::UInt8(u8::MAX)
    );
    pair_tests!(
        i32_to_u64_ok,
        DataType::Int32,
        DataType::UInt64,
        Value::Int32(7),
        Value::UInt64(7),
        Value::Int32(-1),
        Value::UInt64(0)
    );
    pair_tests!(
        i32_to_u32_ok,
        DataType::Int32,
        DataType::UInt32,
        Value::Int32(7),
        Value::UInt32(7),
        Value::Int32(-1),
        Value::UInt32(0)
    );
    pair_tests!(
        i32_to_u16_ok,
        DataType::Int32,
        DataType::UInt16,
        Value::Int32(7),
        Value::UInt16(7),
        Value::Int32(u16::MAX as i32 + 1),
        Value::UInt16(u16::MAX)
    );
    pair_tests!(
        i32_to_u8_ok,
        DataType::Int32,
        DataType::UInt8,
        Value::Int32(7),
        Value::UInt8(7),
        Value::Int32(-1),
        Value::UInt8(0)
    );
    pair_tests!(
        i16_to_u64_ok,
        DataType::Int16,
        DataType::UInt64,
        Value::Int16(7),
        Value::UInt64(7),
        Value::Int16(-1),
        Value::UInt64(0)
    );
    pair_tests!(
        i16_to_u32_ok,
        DataType::Int16,
        DataType::UInt32,
        Value::Int16(7),
        Value::UInt32(7),
        Value::Int16(-1),
        Value::UInt32(0)
    );
    pair_tests!(
        i16_to_u16_ok,
        DataType::Int16,
        DataType::UInt16,
        Value::Int16(7),
        Value::UInt16(7),
        Value::Int16(-1),
        Value::UInt16(0)
    );
    pair_tests!(
        i16_to_u8_ok,
        DataType::Int16,
        DataType::UInt8,
        Value::Int16(7),
        Value::UInt8(7),
        Value::Int16(300),
        Value::UInt8(u8::MAX)
    );

    // Unsigned → signed: saturate at signed max.
    pair_tests!(
        u64_to_i64_ok,
        DataType::UInt64,
        DataType::Int64,
        Value::UInt64(7),
        Value::Int64(7),
        Value::UInt64(u64::MAX),
        Value::Int64(i64::MAX)
    );
    pair_tests!(
        u64_to_i32_ok,
        DataType::UInt64,
        DataType::Int32,
        Value::UInt64(7),
        Value::Int32(7),
        Value::UInt64(i32::MAX as u64 + 1),
        Value::Int32(i32::MAX)
    );
    pair_tests!(
        u64_to_i16_ok,
        DataType::UInt64,
        DataType::Int16,
        Value::UInt64(7),
        Value::Int16(7),
        Value::UInt64(i16::MAX as u64 + 1),
        Value::Int16(i16::MAX)
    );
    pair_tests!(
        u32_to_i32_ok,
        DataType::UInt32,
        DataType::Int32,
        Value::UInt32(7),
        Value::Int32(7),
        Value::UInt32(u32::MAX),
        Value::Int32(i32::MAX)
    );
    pair_tests!(
        u32_to_i16_ok,
        DataType::UInt32,
        DataType::Int16,
        Value::UInt32(7),
        Value::Int16(7),
        Value::UInt32(i16::MAX as u32 + 1),
        Value::Int16(i16::MAX)
    );
    pair_tests!(
        u16_to_i16_ok,
        DataType::UInt16,
        DataType::Int16,
        Value::UInt16(7),
        Value::Int16(7),
        Value::UInt16(u16::MAX),
        Value::Int16(i16::MAX)
    );

    #[test]
    fn unsupported_pair_rejected() {
        // Not a narrowing pair handled here — expect Unsupported fallthrough.
        let res = convert(Value::Bool(true), &DataType::Bool, &DataType::Int32);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    /// Every narrowing arm has its own `_ => Err(ValueShapeMismatch)` branch.
    /// Walk the full src×dst lattice with a deliberately-wrong-variant value
    /// (`Value::Bool(true)`) and assert each arm rejects it instead of
    /// panicking on a wrong cast.
    #[test]
    fn value_shape_mismatch_on_every_arm() {
        let pairs = [
            // signed → smaller signed
            (DataType::Int64, DataType::Int32),
            (DataType::Int64, DataType::Int16),
            (DataType::Int32, DataType::Int16),
            // unsigned → smaller unsigned
            (DataType::UInt64, DataType::UInt32),
            (DataType::UInt64, DataType::UInt16),
            (DataType::UInt64, DataType::UInt8),
            (DataType::UInt32, DataType::UInt16),
            (DataType::UInt32, DataType::UInt8),
            (DataType::UInt16, DataType::UInt8),
            // signed → unsigned
            (DataType::Int64, DataType::UInt64),
            (DataType::Int64, DataType::UInt32),
            (DataType::Int64, DataType::UInt16),
            (DataType::Int64, DataType::UInt8),
            (DataType::Int32, DataType::UInt64),
            (DataType::Int32, DataType::UInt32),
            (DataType::Int32, DataType::UInt16),
            (DataType::Int32, DataType::UInt8),
            (DataType::Int16, DataType::UInt64),
            (DataType::Int16, DataType::UInt32),
            (DataType::Int16, DataType::UInt16),
            (DataType::Int16, DataType::UInt8),
            // unsigned → signed
            (DataType::UInt64, DataType::Int64),
            (DataType::UInt64, DataType::Int32),
            (DataType::UInt64, DataType::Int16),
            (DataType::UInt32, DataType::Int32),
            (DataType::UInt32, DataType::Int16),
            (DataType::UInt16, DataType::Int16),
        ];
        for (src, dst) in pairs {
            let res = convert(Value::Bool(true), &src, &dst);
            assert!(
                matches!(res, Err(ConvertError::ValueShapeMismatch { .. })),
                "expected ValueShapeMismatch for {src:?} → {dst:?}, got {res:?}"
            );
        }
    }
}
