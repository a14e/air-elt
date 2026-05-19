//! `Float{64,32} → Float32` (saturate to f32::MAX/MIN, NaN preserved) and
//! `Float{64,32} → Int*` / `UInt*` (truncate-toward-zero, then saturate).
//! NaN → integer is rejected as `Overflow`. `Float32` sources widen
//! losslessly to `f64` before dispatching to the saturating primitives,
//! so the per-width logic is shared.

use super::error::ConvertError;
use super::saturate::*;
use crate::{DataType, Value};

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;
    let n = match (src, &value) {
        (Float64, Value::Float64(n)) => *n,
        (Float32, Value::Float32(n)) => *n as f64,
        (Float64 | Float32, _) => {
            return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
        }
        _ => {
            return Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: dst.clone(),
            });
        }
    };

    match dst {
        Float32 => Ok(Value::Float32(sat_f64_to_f32(n))),
        Int64 => sat_f64_to_i64(n)
            .map(Value::Int64)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        Int32 => sat_f64_to_i32(n)
            .map(Value::Int32)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        Int16 => sat_f64_to_i16(n)
            .map(Value::Int16)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        Int8 => sat_f64_to_i8(n)
            .map(Value::Int8)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        UInt64 => sat_f64_to_u64(n)
            .map(Value::UInt64)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        UInt32 => sat_f64_to_u32(n)
            .map(Value::UInt32)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        UInt16 => sat_f64_to_u16(n)
            .map(Value::UInt16)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        UInt8 => sat_f64_to_u8(n)
            .map(Value::UInt8)
            .ok_or(ConvertError::Overflow { dst: dst.clone() }),
        _ => Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn f64_to_f32_truncates_toward_zero_or_saturates() {
        assert_eq!(
            convert(Value::Float64(1.5), &DataType::Float64, &DataType::Float32).unwrap(),
            Value::Float32(1.5)
        );
        assert_eq!(
            convert(
                Value::Float64(f64::MAX),
                &DataType::Float64,
                &DataType::Float32
            )
            .unwrap(),
            Value::Float32(f32::MAX)
        );
    }

    #[test]
    fn f64_to_signed_ints_each_width() {
        for (dst, expected) in [
            (DataType::Int64, Value::Int64(7)),
            (DataType::Int32, Value::Int32(7)),
            (DataType::Int16, Value::Int16(7)),
        ] {
            let out = convert(Value::Float64(7.9), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn f64_to_unsigned_ints_each_width() {
        for (dst, expected) in [
            (DataType::UInt64, Value::UInt64(7)),
            (DataType::UInt32, Value::UInt32(7)),
            (DataType::UInt16, Value::UInt16(7)),
            (DataType::UInt8, Value::UInt8(7)),
        ] {
            let out = convert(Value::Float64(7.9), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected);
        }
    }

    #[test]
    fn f64_negative_to_unsigned_clamps_to_zero() {
        // Pin the exact target variant — a swapped arm that produced
        // `UInt64(0)` for a `UInt8` request would otherwise pass.
        for (dst, expected) in [
            (DataType::UInt64, Value::UInt64(0)),
            (DataType::UInt32, Value::UInt32(0)),
            (DataType::UInt16, Value::UInt16(0)),
            (DataType::UInt8, Value::UInt8(0)),
        ] {
            let out = convert(Value::Float64(-1.5), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected, "dst={dst:?}");
        }
    }

    #[test]
    fn f64_per_width_overflow_saturates() {
        // An input that fits Int32 but overflows Int16, etc. — proves each
        // arm dispatches to its own width-specific saturating primitive.
        for (input, dst, expected) in [
            (40_000.0_f64, DataType::Int16, Value::Int16(i16::MAX)),
            (1e10_f64, DataType::Int32, Value::Int32(i32::MAX)),
            (70_000.0_f64, DataType::UInt16, Value::UInt16(u16::MAX)),
            (1e10_f64, DataType::UInt32, Value::UInt32(u32::MAX)),
            (300.0_f64, DataType::UInt8, Value::UInt8(u8::MAX)),
        ] {
            let out = convert(Value::Float64(input), &DataType::Float64, &dst).unwrap();
            assert_eq!(out, expected, "input {input} → {dst:?}");
        }
    }

    #[test]
    fn nan_overflows_for_every_int_target() {
        for dst in [
            DataType::Int64,
            DataType::Int32,
            DataType::Int16,
            DataType::UInt64,
            DataType::UInt32,
            DataType::UInt16,
            DataType::UInt8,
        ] {
            let res = convert(Value::Float64(f64::NAN), &DataType::Float64, &dst);
            assert!(matches!(res, Err(ConvertError::Overflow { .. })), "{dst:?}");
        }
    }

    #[test]
    fn infinity_saturates_to_max() {
        let out = convert(
            Value::Float64(f64::INFINITY),
            &DataType::Float64,
            &DataType::Int64,
        )
        .unwrap();
        assert_eq!(out, Value::Int64(i64::MAX));
        let out = convert(
            Value::Float64(f64::NEG_INFINITY),
            &DataType::Float64,
            &DataType::Int64,
        )
        .unwrap();
        assert_eq!(out, Value::Int64(i64::MIN));
    }

    #[test]
    fn value_shape_mismatch_rejected() {
        let res = convert(Value::Int32(1), &DataType::Float64, &DataType::Int32);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    /// Float32 source widens to f64 internally; per-width saturation is
    /// shared with the Float64 path. Pin both signed and unsigned arms.
    #[test]
    fn f32_to_int_each_width() {
        for (dst, expected) in [
            (DataType::Int16, Value::Int16(7)),
            (DataType::Int32, Value::Int32(7)),
            (DataType::Int64, Value::Int64(7)),
            (DataType::UInt8, Value::UInt8(7)),
            (DataType::UInt32, Value::UInt32(7)),
        ] {
            let out = convert(Value::Float32(7.9_f32), &DataType::Float32, &dst).unwrap();
            assert_eq!(out, expected);
        }
    }

    /// Float32 NaN → integer rejected as Overflow, same as Float64.
    #[test]
    fn f32_nan_overflows_for_int() {
        let res = convert(
            Value::Float32(f32::NAN),
            &DataType::Float32,
            &DataType::Int32,
        );
        assert!(matches!(res, Err(ConvertError::Overflow { .. })));
    }

    /// Float32 source MUST carry a `Value::Float32`; a `Value::Float64`
    /// payload tagged with `Float32` source is a shape mismatch.
    #[test]
    fn f32_source_with_wrong_value_shape_rejected() {
        let res = convert(Value::Float64(1.0), &DataType::Float32, &DataType::Int32);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn unsupported_target_rejected() {
        let res = convert(Value::Float64(1.0), &DataType::Float64, &DataType::Bool);
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// NaN input must surface `Overflow` for *every* integer target —
    /// signed and unsigned, every width.
    #[test_strategy::proptest(ProptestConfig::with_cases(32))]
    fn float_narrow_nan_to_int_overflows(
        #[strategy(prop_oneof![
            Just(DataType::Int8),
            Just(DataType::Int16),
            Just(DataType::Int32),
            Just(DataType::Int64),
            Just(DataType::UInt8),
            Just(DataType::UInt16),
            Just(DataType::UInt32),
            Just(DataType::UInt64),
        ])]
        dst: DataType,
    ) {
        let res = convert(Value::Float64(f64::NAN), &DataType::Float64, &dst);
        let is_overflow = matches!(res, Err(ConvertError::Overflow { .. }));
        prop_assert!(is_overflow);
        // Same for Float32 NaN.
        let res_f32 = convert(Value::Float32(f32::NAN), &DataType::Float32, &dst);
        let is_overflow_f32 = matches!(res_f32, Err(ConvertError::Overflow { .. }));
        prop_assert!(is_overflow_f32);
    }

    /// Subnormal floats (smaller-than-`f32::MIN_POSITIVE` magnitudes that
    /// are still finite) must preserve sign when narrowed `f64 → f32`.
    /// We assert on `is_sign_negative()` / `is_sign_positive()` rather
    /// than `<= 0.0` / `>= 0.0` because the IEEE-754 ordering operator
    /// treats `+0.0 == -0.0`: a bug that flushed a negative subnormal
    /// to `+0.0` would pass `f <= 0.0`. `is_sign_negative()` inspects
    /// the sign bit directly and distinguishes `-0.0` from `+0.0`.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn float_narrow_subnormal_preserves_sign(
        #[strategy(1u32..=20u32)] shift: u32,
        #[strategy(any::<bool>())] negative: bool,
    ) {
        // Construct a subnormal-ish f64 below f32::MIN_POSITIVE.
        let magnitude = f32::MIN_POSITIVE as f64 / (1u64 << shift) as f64;
        let value = if negative { -magnitude } else { magnitude };
        let out = convert(
            Value::Float64(value),
            &DataType::Float64,
            &DataType::Float32,
        )
        .expect("convert");
        match out {
            Value::Float32(f) => {
                // Either f is also subnormal-or-zero with matching sign,
                // or it flushed to zero — in which case the sign bit of
                // the resulting zero must still match the input.
                if negative {
                    prop_assert!(
                        f.is_sign_negative(),
                        "expected negative sign bit, got {f} (bits={:#x})",
                        f.to_bits()
                    );
                } else {
                    prop_assert!(
                        f.is_sign_positive(),
                        "expected positive sign bit, got {f} (bits={:#x})",
                        f.to_bits()
                    );
                }
            }
            _ => prop_assert!(false, "expected Float32"),
        }
    }

    /// `Float64 → Float32` saturates magnitudes above `+f32::MAX` to
    /// `f32::MAX` rather than `+Infinity` under truncate semantics.
    #[test]
    fn float64_to_float32_positive_overflow_saturates_to_f32_max() {
        // 1e40 sits comfortably above f32::MAX (~3.4e38) and well
        // inside f64::MAX.
        let out = convert(
            Value::Float64(1e40_f64),
            &DataType::Float64,
            &DataType::Float32,
        )
        .unwrap();
        assert_eq!(out, Value::Float32(f32::MAX));
    }

    /// `Float64 → Float32` saturates magnitudes below `-f32::MAX` to
    /// `f32::MIN` rather than `-Infinity` under truncate semantics.
    #[test]
    fn float64_to_float32_negative_overflow_saturates_to_f32_min() {
        let out = convert(
            Value::Float64(-1e40_f64),
            &DataType::Float64,
            &DataType::Float32,
        )
        .unwrap();
        assert_eq!(out, Value::Float32(f32::MIN));
    }
}
