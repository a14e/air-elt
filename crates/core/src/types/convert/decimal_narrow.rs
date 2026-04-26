//! `Decimal → Decimal(p, s)` (drop scale + saturate precision),
//! `Decimal → BigInt(width)` (drop scale + saturate width), and
//! `Decimal → Int*/UInt*` (drop scale + saturate range). All routes
//! truncate toward zero; over-range integer parts saturate at min/max.

use super::error::ConvertError;
use super::saturate::*;
use crate::types::{DataType, Value};
use bigdecimal::BigDecimal;

pub fn convert(value: Value, src: &DataType, dst: &DataType) -> Result<Value, ConvertError> {
    use DataType::*;
    let d = match value {
        Value::Decimal(d) => d,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
    };
    match dst {
        Decimal { precision, scale } => Ok(Value::Decimal(narrow_decimal(d, *precision, *scale))),
        BigInt { width } => {
            let b = bigdecimal_to_bigint_truncating(&d);
            let saturated = match width {
                Some(w) => sat_bigint_to_width(&b, *w),
                None => b,
            };
            Ok(Value::BigInt(saturated))
        }
        Int64 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int64(sat_bigint_to_i64(&b)))
        }
        Int32 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int32(sat_bigint_to_i32(&b)))
        }
        Int16 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::Int16(sat_bigint_to_i16(&b)))
        }
        UInt64 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt64(sat_bigint_to_u64(&b)))
        }
        UInt32 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt32(sat_bigint_to_u32(&b)))
        }
        UInt16 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt16(sat_bigint_to_u16(&b)))
        }
        UInt8 => {
            let b = bigdecimal_to_bigint_truncating(&d);
            Ok(Value::UInt8(sat_bigint_to_u8(&b)))
        }
        _ => Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
        }),
    }
}

fn narrow_decimal(d: BigDecimal, precision: Option<u32>, scale: Option<u32>) -> BigDecimal {
    // 1) reduce scale (truncate toward zero — `with_scale` does this).
    let scale_to_use = scale.map(|s| s as i64);
    let mut out = match scale_to_use {
        Some(target_scale) => d.with_scale(target_scale),
        None => d,
    };
    // 2) saturate to fit `precision` integer-digits given the now-fixed scale.
    if let Some(p) = precision {
        let s = scale.unwrap_or(0);
        let int_digits = p.saturating_sub(s);
        let (mantissa, mantissa_scale) = out.into_bigint_and_exponent();
        let saturated = sat_bigint_to_width(&mantissa, p);
        // p covers integer + fractional digits combined when reconstructing
        // with the original scale; sat_bigint_to_width caps |mantissa| <
        // 10^p which is the BigDecimal's underlying invariant.
        let _ = int_digits; // logically used; saturation covers both halves.
        out = BigDecimal::new(saturated, mantissa_scale);
    }
    out
}
