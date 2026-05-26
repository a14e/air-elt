//! `(src, dst, ctx)` dispatcher for value conversion. See module docs.
//!
//! Becomes a thin router: identity / pure-widening short-circuits stay
//! here; everything narrowing-or-cross-type is delegated to a per-group
//! submodule (`int_narrow`, `text_narrow`, `json_text`, `xml`, etc.).
//! Truncate-only paths require `ctx.truncate=true`; explicitly-forbidden
//! truncate combinations return [`ConvertError::TruncationForbidden`].

use super::ConvertError;
use super::context::ConversionContext;
use super::{
    bigint_narrow, bytes_narrow, decimal_narrow, decimal_to_float, float_narrow, int_narrow, ip,
    json_text, text_bool, text_narrow, timestamp_date, uuid as uuid_conv, xml,
};
use crate::{DataType, Value};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use num_traits::Zero;

pub fn convert(
    value: Value,
    src: &DataType,
    dst: &DataType,
    ctx: &ConversionContext,
) -> Result<Value, ConvertError> {
    // Null substitution via default. After substitution the value is already
    // in sink type — return it directly.
    if matches!(value, Value::Null) {
        if let Some(def) = &ctx.default {
            return Ok(def.clone());
        }
        return Ok(Value::Null);
    }

    // Union source: pick the concrete source type from the actual value
    // variant and re-dispatch. The matrix has already approved every
    // member of the union against the sink, so this is just runtime
    // routing.
    if matches!(src, DataType::Union(_)) {
        let concrete = value
            .data_type()
            .ok_or_else(|| ConvertError::ValueShapeMismatch { src: src.clone() })?;
        return convert(value, &concrete, dst, ctx);
    }

    // Custom routing: opaque types own their conversion logic; the
    // dispatcher delegates without inspecting the value variant. Both-
    // sides Custom is identity iff the descriptors match.
    if let (DataType::Custom(a), DataType::Custom(b)) = (src, dst) {
        if a.is_equal(&**b) {
            return Ok(value);
        }
        return Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        });
    }
    if let DataType::Custom(a) = src {
        return a.convert(value, dst, ctx);
    }
    if let DataType::Custom(b) = dst {
        return b.construct(value, src, ctx);
    }

    if src == dst {
        return identity_or_forbid(value, src, dst, ctx);
    }

    // Pure-widening short-circuits where the value variant is the same
    // across the type (Text/Bytes carry the same payload). For BigInt/
    // Decimal we use the same shape but width/precision changes — narrowing
    // is gated by `ctx.truncate` and goes through the dedicated module.
    if let (DataType::Text { size: a }, DataType::Text { size: b }) = (src, dst) {
        if widens_or_equal(*a, *b) {
            return Ok(value);
        }
        if ctx.truncate {
            return text_narrow::convert(value, src, *b);
        }
        return Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        });
    }
    if let (DataType::Bytes { size: a }, DataType::Bytes { size: b }) = (src, dst) {
        if widens_or_equal(*a, *b) {
            return Ok(value);
        }
        if ctx.truncate {
            return bytes_narrow::convert(value, src, *b);
        }
        return Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        });
    }

    match (src, dst) {
        // ---- Identity-ish for parametric types --------------------------
        (DataType::BigInt { .. }, DataType::BigInt { .. })
        | (DataType::Decimal { .. }, DataType::Decimal { .. }) => {
            identity_or_narrow_numeric(value, src, dst, ctx)
        }

        // ---- UUID round-trips (existing semantics) ---------------------
        (DataType::Uuid, DataType::Text { .. }) => match value {
            Value::Uuid(u) => Ok(Value::Text(uuid_conv::to_text(u))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Uuid, DataType::Bytes { .. }) => match value {
            Value::Uuid(u) => Ok(Value::Bytes(uuid_conv::to_bytes(u).to_vec())),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Text { .. }, DataType::Uuid) => match value {
            Value::Text(s) => Ok(Value::Uuid(uuid_conv::parse_text(&s)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bytes { .. }, DataType::Uuid) => match value {
            Value::Bytes(b) => Ok(Value::Uuid(uuid_conv::from_bytes(&b)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- IPv4 / IPv6 round-trips ----------------------------------
        // Ipv4 → Ipv6 is always lossless (IPv4-mapped widening).
        (DataType::Ipv4, DataType::Ipv6) => match value {
            Value::Ipv4(a) => Ok(Value::Ipv6(ip::v4_to_v6(a))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        // Ipv6 → Ipv4 only when the source is IPv4-mapped. The matrix
        // admits this pair only under `truncate=true`; here we still
        // guard so a caller bypassing the matrix gets a clean error.
        (DataType::Ipv6, DataType::Ipv4) => {
            if !ctx.truncate {
                return Err(ConvertError::Unsupported {
                    src: src.clone(),
                    dst: dst.clone(),
                });
            }
            match value {
                Value::Ipv6(a) => Ok(Value::Ipv4(ip::v6_to_v4_if_mapped(a)?)),
                _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            }
        }
        (DataType::Ipv4, DataType::Text { .. }) => match value {
            Value::Ipv4(a) => Ok(Value::Text(ip::to_text_v4(a))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Ipv6, DataType::Text { .. }) => match value {
            Value::Ipv6(a) => Ok(Value::Text(ip::to_text_v6(a))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Text { .. }, DataType::Ipv4) => match value {
            Value::Text(s) => Ok(Value::Ipv4(ip::parse_v4(&s)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Text { .. }, DataType::Ipv6) => match value {
            Value::Text(s) => Ok(Value::Ipv6(ip::parse_v6(&s)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Ipv4, DataType::Bytes { .. }) => match value {
            Value::Ipv4(a) => Ok(Value::Bytes(ip::to_bytes_v4(a).to_vec())),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Ipv6, DataType::Bytes { .. }) => match value {
            Value::Ipv6(a) => Ok(Value::Bytes(ip::to_bytes_v6(a).to_vec())),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bytes { .. }, DataType::Ipv4) => match value {
            Value::Bytes(b) => Ok(Value::Ipv4(ip::from_bytes_v4(&b)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bytes { .. }, DataType::Ipv6) => match value {
            Value::Bytes(b) => Ok(Value::Ipv6(ip::from_bytes_v6(&b)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- Int8 widening --------------------------------------------
        (DataType::Int8, DataType::Int16) => match value {
            Value::Int8(n) => Ok(Value::Int16(n as i16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int8, DataType::Int32) => match value {
            Value::Int8(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int8, DataType::Int64) => match value {
            Value::Int8(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int8, DataType::Float32) => match value {
            Value::Int8(n) => Ok(Value::Float32(n as f32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int8, DataType::Float64) => match value {
            Value::Int8(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int8, DataType::BigInt { .. }) => match value {
            Value::Int8(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int8, DataType::Decimal { .. }) => match value {
            Value::Int8(n) => Ok(Value::Decimal(BigDecimal::from(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- Int / UInt ↔ Bool (existing) -----------------------------
        (DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64, DataType::Bool) => {
            let n: i64 = match value {
                Value::Int8(n) => n as i64,
                Value::Int16(n) => n as i64,
                Value::Int32(n) => n as i64,
                Value::Int64(n) => n,
                _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
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
                _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            };
            Ok(Value::Bool(n != 0))
        }
        (DataType::Bool, DataType::UInt8) => match value {
            Value::Bool(b) => Ok(Value::UInt8(b as u8)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::UInt16) => match value {
            Value::Bool(b) => Ok(Value::UInt16(b as u16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::UInt32) => match value {
            Value::Bool(b) => Ok(Value::UInt32(b as u32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::UInt64) => match value {
            Value::Bool(b) => Ok(Value::UInt64(b as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::Int8) => match value {
            Value::Bool(b) => Ok(Value::Int8(b as i8)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::Int16) => match value {
            Value::Bool(b) => Ok(Value::Int16(b as i16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::Int32) => match value {
            Value::Bool(b) => Ok(Value::Int32(b as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::Int64) => match value {
            Value::Bool(b) => Ok(Value::Int64(b as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- Bool → Float / BigInt / Decimal (lossless) ---------------
        // `false → 0`, `true → 1`. Exact in IEEE-754 and any fixed-point
        // representation. No truncate gate required.
        (DataType::Bool, DataType::Float32) => match value {
            Value::Bool(b) => Ok(Value::Float32(if b { 1.0 } else { 0.0 })),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::Float64) => match value {
            Value::Bool(b) => Ok(Value::Float64(if b { 1.0 } else { 0.0 })),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::BigInt { .. }) => match value {
            Value::Bool(b) => Ok(Value::BigInt(BigInt::from(u8::from(b)))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Bool, DataType::Decimal { .. }) => match value {
            Value::Bool(b) => Ok(Value::Decimal(BigDecimal::from(u8::from(b)))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- BigInt / Decimal → Bool (truncate-only) ------------------
        // `!is_zero()`. Matrix admits these only under `truncate=true`;
        // runtime defends defensively in case a caller bypassed validation.
        (DataType::BigInt { .. }, DataType::Bool) => {
            if !ctx.truncate {
                return Err(ConvertError::Unsupported {
                    src: src.clone(),
                    dst: dst.clone(),
                });
            }
            match value {
                Value::BigInt(n) => Ok(Value::Bool(!Zero::is_zero(&n))),
                _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            }
        }
        (DataType::Decimal { .. }, DataType::Bool) => {
            if !ctx.truncate {
                return Err(ConvertError::Unsupported {
                    src: src.clone(),
                    dst: dst.clone(),
                });
            }
            match value {
                Value::Decimal(d) => Ok(Value::Bool(!Zero::is_zero(&d))),
                _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            }
        }

        // ---- Float* → Bool (truncate-only, nullable) ------------------
        // `f == 0.0` covers ±0.0 by IEEE equality. `NaN` is the absence of
        // a meaningful value — return `ctx.default` if the operator
        // provided one, else `Value::Null` (the validation pipeline gates
        // this latter case on the sink column being nullable). `±Inf` and
        // every other non-zero finite float maps to `true`.
        (DataType::Float32 | DataType::Float64, DataType::Bool) => {
            if !ctx.truncate {
                return Err(ConvertError::Unsupported {
                    src: src.clone(),
                    dst: dst.clone(),
                });
            }
            let f: f64 = match value {
                Value::Float32(f) => f as f64,
                Value::Float64(f) => f,
                _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            };
            if f.is_nan() {
                return Ok(ctx.default.clone().unwrap_or(Value::Null));
            }
            Ok(Value::Bool(f != 0.0))
        }

        // ---- Text → Bool lexer (always allowed, no truncate gate) ------
        (DataType::Text { .. }, DataType::Bool) => text_bool::convert(value, src),

        // ---- Numeric widening (existing) ------------------------------
        (DataType::Int16, DataType::Int32) => match value {
            Value::Int16(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int16, DataType::Int64) => match value {
            Value::Int16(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int32, DataType::Int64) => match value {
            Value::Int32(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int16, DataType::Float32) => match value {
            Value::Int16(n) => Ok(Value::Float32(n as f32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int16, DataType::Float64) => match value {
            Value::Int16(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int32, DataType::Float64) => match value {
            Value::Int32(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Float32, DataType::Float64) => match value {
            Value::Float32(n) => Ok(Value::Float64(n as f64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // Float narrowing — only with truncate. Float32 widens
        // losslessly to f64 inside `float_narrow::convert`, so the
        // per-width saturation logic is shared.
        (DataType::Float64, DataType::Float32)
        | (DataType::Float64, DataType::Int64)
        | (DataType::Float64, DataType::Int32)
        | (DataType::Float64, DataType::Int16)
        | (DataType::Float64, DataType::Int8)
        | (DataType::Float64, DataType::UInt64)
        | (DataType::Float64, DataType::UInt32)
        | (DataType::Float64, DataType::UInt16)
        | (DataType::Float64, DataType::UInt8)
        | (DataType::Float32, DataType::Int64)
        | (DataType::Float32, DataType::Int32)
        | (DataType::Float32, DataType::Int16)
        | (DataType::Float32, DataType::Int8)
        | (DataType::Float32, DataType::UInt64)
        | (DataType::Float32, DataType::UInt32)
        | (DataType::Float32, DataType::UInt16)
        | (DataType::Float32, DataType::UInt8) => {
            require_truncate(ctx, src, dst)?;
            float_narrow::convert(value, src, dst)
        }

        // Fixed-width int → BigInt.
        (DataType::Int16, DataType::BigInt { .. }) => match value {
            Value::Int16(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int32, DataType::BigInt { .. }) => match value {
            Value::Int32(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int64, DataType::BigInt { .. }) => match value {
            Value::Int64(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // Fixed-width int → Decimal.
        (DataType::Int16, DataType::Decimal { .. }) => match value {
            Value::Int16(n) => Ok(Value::Decimal(BigDecimal::from(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int32, DataType::Decimal { .. }) => match value {
            Value::Int32(n) => Ok(Value::Decimal(BigDecimal::from(n as i64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::Int64, DataType::Decimal { .. }) => match value {
            Value::Int64(n) => Ok(Value::Decimal(BigDecimal::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::BigInt { .. }, DataType::Decimal { .. }) => match value {
            Value::BigInt(b) => Ok(Value::Decimal(BigDecimal::new(b, 0))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // Unsigned widening within unsigned.
        (DataType::UInt8, DataType::UInt16) => match value {
            Value::UInt8(n) => Ok(Value::UInt16(n as u16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt8, DataType::UInt32) => match value {
            Value::UInt8(n) => Ok(Value::UInt32(n as u32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt8, DataType::UInt64) => match value {
            Value::UInt8(n) => Ok(Value::UInt64(n as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt16, DataType::UInt32) => match value {
            Value::UInt16(n) => Ok(Value::UInt32(n as u32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt16, DataType::UInt64) => match value {
            Value::UInt16(n) => Ok(Value::UInt64(n as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt32, DataType::UInt64) => match value {
            Value::UInt32(n) => Ok(Value::UInt64(n as u64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // Unsigned → signed (matrix already enforces width fits).
        (DataType::UInt8, DataType::Int16) => match value {
            Value::UInt8(n) => Ok(Value::Int16(n as i16)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt8, DataType::Int32) => match value {
            Value::UInt8(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt8, DataType::Int64) => match value {
            Value::UInt8(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt16, DataType::Int32) => match value {
            Value::UInt16(n) => Ok(Value::Int32(n as i32)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt16, DataType::Int64) => match value {
            Value::UInt16(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt32, DataType::Int64) => match value {
            Value::UInt32(n) => Ok(Value::Int64(n as i64)),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // Unsigned → BigInt.
        (DataType::UInt8, DataType::BigInt { .. }) => match value {
            Value::UInt8(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt16, DataType::BigInt { .. }) => match value {
            Value::UInt16(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt32, DataType::BigInt { .. }) => match value {
            Value::UInt32(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt64, DataType::BigInt { .. }) => match value {
            Value::UInt64(n) => Ok(Value::BigInt(BigInt::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // Unsigned → Decimal.
        (DataType::UInt8, DataType::Decimal { .. }) => match value {
            Value::UInt8(n) => Ok(Value::Decimal(BigDecimal::from(n as u64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt16, DataType::Decimal { .. }) => match value {
            Value::UInt16(n) => Ok(Value::Decimal(BigDecimal::from(n as u64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt32, DataType::Decimal { .. }) => match value {
            Value::UInt32(n) => Ok(Value::Decimal(BigDecimal::from(n as u64))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },
        (DataType::UInt64, DataType::Decimal { .. }) => match value {
            Value::UInt64(n) => Ok(Value::Decimal(BigDecimal::from(n))),
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- Integer narrowing (signed/unsigned/cross-sign) ------------
        (DataType::Int64, DataType::Int32 | DataType::Int16 | DataType::Int8)
        | (DataType::Int32, DataType::Int16 | DataType::Int8)
        | (DataType::Int16, DataType::Int8)
        | (DataType::UInt64, DataType::UInt32 | DataType::UInt16 | DataType::UInt8)
        | (DataType::UInt32, DataType::UInt16 | DataType::UInt8)
        | (DataType::UInt16, DataType::UInt8)
        | (
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64,
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64,
        )
        | (
            DataType::UInt64,
            DataType::Int64 | DataType::Int32 | DataType::Int16 | DataType::Int8,
        )
        | (DataType::UInt32, DataType::Int32 | DataType::Int16 | DataType::Int8)
        | (DataType::UInt16, DataType::Int16 | DataType::Int8)
        | (DataType::UInt8, DataType::Int8) => {
            require_truncate(ctx, src, dst)?;
            int_narrow::convert(value, src, dst)
        }

        // ---- BigInt narrowing → Int*/UInt* ----------------------------
        (
            DataType::BigInt { .. },
            DataType::Int64
            | DataType::Int32
            | DataType::Int16
            | DataType::Int8
            | DataType::UInt64
            | DataType::UInt32
            | DataType::UInt16
            | DataType::UInt8,
        ) => {
            require_truncate(ctx, src, dst)?;
            bigint_narrow::convert(value, src, dst)
        }

        // ---- Decimal → Float64 / Float32 ------------------------------
        //
        // Conversion via `BigDecimal::to_f64()` then `as f32` for the
        // 32-bit arm. Loss happens in two distinct shapes:
        //
        //   1. **Mantissa rounding** when the decimal value carries more
        //      significant digits than the target float can represent
        //      exactly (Float64: ~15 digits, Float32: ~7). The matrix
        //      already classifies this kind of pair as narrowing — the
        //      matrix admits the lossless flavour only when
        //      `precision <= 15` (Float64) / `precision <= 7` (Float32)
        //      and both precision/scale are known. Beyond that, the
        //      validator gates on `truncate=true`, and *that* `truncate`
        //      flag is what makes `ctx.truncate` true here. The
        //      dispatcher itself does not re-check the precision rule —
        //      `decimal_to_float` returns whatever `to_f64()` produces
        //      and any mantissa loss is silently absorbed by the IEEE
        //      cast, mirroring the existing Float→Float narrowing
        //      semantics in `float_narrow`.
        //
        //   2. **Magnitude overflow** when the value exceeds the target
        //      float's range (Float64: ~1.8e308, Float32: ~3.4e38). Here
        //      `truncate=true` saturates to `±INFINITY` matching the
        //      value's sign; `truncate=false` surfaces `Overflow` so
        //      the operator sees the loss rather than a silent `inf`.
        (DataType::Decimal { .. }, DataType::Float64) => {
            decimal_to_float::convert_to_f64(value, src, ctx.truncate)
        }
        (DataType::Decimal { .. }, DataType::Float32) => {
            decimal_to_float::convert_to_f32(value, src, ctx.truncate)
        }

        // ---- Decimal narrowing → BigInt/Int*/UInt* --------------------
        (
            DataType::Decimal { .. },
            DataType::BigInt { .. }
            | DataType::Int64
            | DataType::Int32
            | DataType::Int16
            | DataType::Int8
            | DataType::UInt64
            | DataType::UInt32
            | DataType::UInt16
            | DataType::UInt8,
        ) => {
            require_truncate(ctx, src, dst)?;
            decimal_narrow::convert(value, src, dst)
        }

        // ---- Json → Text ---------------------------------------------
        (DataType::Json, DataType::Text { size }) => {
            // Bounded sink requires consent; unbounded does not.
            if size.is_some() {
                require_truncate(ctx, src, dst)?;
            }
            json_text::convert(value, src, *size)
        }

        // ---- Xml → Text / Text → Xml --------------------------------
        (DataType::Xml, DataType::Text { size }) => {
            if size.is_some() {
                require_truncate(ctx, src, dst)?;
            }
            xml::xml_to_text(value, src, *size)
        }
        (DataType::Text { .. }, DataType::Xml) => xml::text_to_xml(value, src),

        // ---- Timestamp → Date ---------------------------------------
        (DataType::Timestamp, DataType::Date) => {
            require_truncate(ctx, src, dst)?;
            timestamp_date::convert(value, src)
        }

        // ---- Object → Json -------------------------------------------
        (DataType::Object, DataType::Json) => match value {
            Value::Object(entries) => {
                let mut map = serde_json::Map::with_capacity(entries.len());
                for (key, val) in entries {
                    let json_val = crate::json_encode::value_to_json(&val).map_err(|e| {
                        ConvertError::Custom {
                            message: format!("object to json: {e}"),
                        }
                    })?;
                    map.insert(key, json_val);
                }
                Ok(Value::Json(serde_json::Value::Object(map)))
            }
            _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
        },

        // ---- Object → Text -------------------------------------------
        (DataType::Object, DataType::Text { size }) => {
            if size.is_some() {
                require_truncate(ctx, src, dst)?;
            }
            match value {
                Value::Object(entries) => {
                    let mut map = serde_json::Map::with_capacity(entries.len());
                    for (key, val) in entries {
                        let json_val = crate::json_encode::value_to_json(&val).map_err(|e| {
                            ConvertError::Custom {
                                message: format!("object to text: {e}"),
                            }
                        })?;
                        map.insert(key, json_val);
                    }
                    let serialized = serde_json::Value::Object(map).to_string();
                    if let Some(max) = size {
                        let chars: String = serialized.chars().take(*max as usize).collect();
                        Ok(Value::Text(chars))
                    } else {
                        Ok(Value::Text(serialized))
                    }
                }
                _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            }
        }

        // ---- Json → Object (truncate-only) ---------------------------
        (DataType::Json, DataType::Object) => {
            require_truncate(ctx, src, dst)?;
            match value {
                Value::Json(serde_json::Value::Object(map)) => {
                    let entries: Vec<(String, Value)> =
                        map.into_iter().map(|(k, v)| (k, Value::Json(v))).collect();
                    Ok(Value::Object(entries))
                }
                Value::Json(_) => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
                _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            }
        }

        _ => Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        }),
    }
}

/// Identity (`src == dst`) check. `Json → Json` / `Xml → Xml` with
/// `truncate=true` are rejected at runtime — truncating structured payloads
/// corrupts the syntax. `Uuid/Date/Timestamp` identity-with-truncate is
/// caught earlier by the matrix at validation; if the dispatcher is invoked
/// directly (bypassing validation) we treat the request as a harmless no-op
/// rather than erroring on data already in transit.
fn identity_or_forbid(
    value: Value,
    src: &DataType,
    dst: &DataType,
    ctx: &ConversionContext,
) -> Result<Value, ConvertError> {
    if ctx.truncate && matches!(src, DataType::Json | DataType::Xml) {
        return Err(ConvertError::TruncationForbidden {
            src: src.clone(),
            dst: dst.clone(),
        });
    }
    Ok(value)
}

fn identity_or_narrow_numeric(
    value: Value,
    src: &DataType,
    dst: &DataType,
    ctx: &ConversionContext,
) -> Result<Value, ConvertError> {
    use DataType::*;
    // BigInt arm keeps the original static rule — widening unconditionally,
    // narrowing only under `truncate=true`.
    if let (BigInt { width: a }, BigInt { width: b }) = (src, dst) {
        if widens_or_equal(*a, *b) {
            return Ok(value);
        }
        if !ctx.truncate {
            return Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: dst.clone(),
            });
        }
        return bigint_narrow::convert(value, src, dst);
    }

    // Decimal arm is value-aware: even when the *static* declared source
    // type is wider than the target (e.g. unbounded `Decimal { None, None }`
    // produced by Mongo's Decimal128 → canonical mapping), the actual
    // `BigDecimal` payload may still fit the target's precision/scale
    // exactly, in which case the conversion is lossless and we do not
    // require the user to opt into `truncate=true`. Only when the value
    // genuinely loses information (fractional digits dropped, integer-part
    // overflow) do we gate behind `truncate`.
    if matches!(src, Decimal { .. }) && matches!(dst, Decimal { .. }) {
        return convert_decimal_to_decimal(value, src, dst, ctx);
    }

    Err(ConvertError::Unsupported {
        src: src.clone(),
        dst: dst.clone(),
    })
}

/// Decimal → Decimal value-aware dispatch.
///
/// The static type matrix may have already approved this conversion under
/// `truncate=true`, but at runtime the actual `BigDecimal` mantissa often
/// fits a narrower target losslessly — in particular the mongo→pg path
/// where every Decimal128 carries `DataType::Decimal { None, None }`
/// regardless of the value's real magnitude. Routing decisions here:
///
/// * **No-op rescale into an unbounded target** (`pb = None, sb = None`):
///   the BigDecimal payload is preserved as-is.
/// * **Always-safe widening**: target scale ≥ source scale AND target
///   integer-digit capacity ≥ source integer-digit count → return the
///   value unchanged. The decision uses the actual value's mantissa width
///   when the source type's bounds are absent.
/// * **Narrowing without `truncate`**: return [`ConvertError::Unsupported`]
///   — the user has not opted into a lossy cast.
/// * **Narrowing with `truncate`**: delegate to [`decimal_narrow::convert`]
///   which rescales via `BigDecimal::with_scale_round(target_scale,
///   RoundingMode::Down)` (truncate toward zero, matching the existing
///   Decimal-scale-narrowing convention shared with Postgres/ClickHouse)
///   and saturates integer-digit overflow on the mantissa.
fn convert_decimal_to_decimal(
    value: Value,
    src: &DataType,
    dst: &DataType,
    ctx: &ConversionContext,
) -> Result<Value, ConvertError> {
    // Caller has guaranteed both sides are `DataType::Decimal { .. }`. Pull
    // the precision/scale tuples out so the rest of the body can reason in
    // terms of plain options.
    let (pa, sa) = match src {
        DataType::Decimal { precision, scale } => (*precision, *scale),
        _ => unreachable!("convert_decimal_to_decimal called with non-Decimal src"),
    };
    let (pb, sb) = match dst {
        DataType::Decimal { precision, scale } => (*precision, *scale),
        _ => unreachable!("convert_decimal_to_decimal called with non-Decimal dst"),
    };

    // Unbounded target soaks up any value losslessly.
    if pb.is_none() && sb.is_none() {
        return Ok(value);
    }

    // Static widening (both sides bounded enough to decide from types alone).
    if decimal_widens_or_equal(pa, sa, pb, sb) {
        return Ok(value);
    }

    // Extract the actual BigDecimal payload to make a value-level decision.
    // Value-shape mismatch is the right error for "src claims Decimal but
    // the variant isn't `Value::Decimal`".
    let d = match &value {
        Value::Decimal(d) => d,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };

    // Per-value widening probe: compute the value's own scale and
    // integer-digit count and see if both fit the target's bounds. When
    // the source type is statically wider than the target this is the
    // mongo→pg lossless path.
    let value_scale = decimal_value_scale(d);
    let value_int_digits = decimal_value_int_digits(d);
    let target_scale = sb.unwrap_or(0);
    let target_int_capacity = match (pb, sb) {
        (None, _) => u32::MAX,
        (Some(p), Some(s)) => p.saturating_sub(s),
        (Some(p), None) => p,
    };
    let fits_scale = u64::from(target_scale) >= value_scale;
    let fits_int = u64::from(target_int_capacity) >= value_int_digits;
    if fits_scale && fits_int {
        // Rescale only when the BigDecimal's stored scale differs from the
        // target's declared scale. The value is unchanged numerically — we
        // just align the on-the-wire scale so the sink-side bind path sees
        // the expected precision/scale shape.
        return Ok(Value::Decimal(d.with_scale_round(
            i64::from(target_scale),
            bigdecimal::RoundingMode::Down,
        )));
    }

    if !ctx.truncate {
        // Lossy: either fractional digits would be dropped or the integer
        // part overflows the target's int-digit capacity. Without consent
        // we refuse — `Unsupported` keeps the error shape consistent with
        // the static type-matrix rejection.
        return Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        });
    }

    decimal_narrow::convert(value, src, dst)
}

/// Number of fractional digits the BigDecimal value carries, ignoring
/// trailing zeros (e.g. "12.30" reports 1, "12.300" still 1). Matches the
/// "significant scale" rule the default-value parser uses so the dispatcher
/// agrees with validation on what counts as widening.
fn decimal_value_scale(d: &BigDecimal) -> u64 {
    let normalized = d.normalized();
    let scale = normalized.fractional_digit_count();
    if scale <= 0 { 0 } else { scale as u64 }
}

/// Number of integer digits in the value, ignoring sign. Zero reports 0.
fn decimal_value_int_digits(d: &BigDecimal) -> u64 {
    let int_part = d.with_scale_round(0, bigdecimal::RoundingMode::Down);
    let (mantissa, _exp) = int_part.into_bigint_and_exponent();
    if mantissa.sign() == num_bigint::Sign::NoSign {
        return 0;
    }
    // BigInt has no direct decimal-digit count; the string form's length
    // (minus a possible leading `-`) is the conventional way and avoids
    // re-implementing log10 on arbitrary-precision integers.
    let s = mantissa.to_string();
    s.trim_start_matches('-').len() as u64
}

fn widens_or_equal(a: Option<u32>, b: Option<u32>) -> bool {
    match (a, b) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(b)) => a <= b,
    }
}

fn decimal_widens_or_equal(
    pa: Option<u32>,
    sa: Option<u32>,
    pb: Option<u32>,
    sb: Option<u32>,
) -> bool {
    match (pa, pb) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(pa), Some(pb)) => {
            let sa = sa.unwrap_or(0);
            let sb = sb.unwrap_or(0);
            sb >= sa && pb.saturating_sub(sb) >= pa.saturating_sub(sa)
        }
    }
}

fn require_truncate(
    ctx: &ConversionContext,
    src: &DataType,
    dst: &DataType,
) -> Result<(), ConvertError> {
    if ctx.truncate {
        Ok(())
    } else {
        Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: dst.clone(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ::uuid::Uuid as UuidVal;

    fn passthrough() -> ConversionContext {
        ConversionContext::passthrough()
    }
    fn truncate_ctx() -> ConversionContext {
        ConversionContext::passthrough().with_truncate()
    }
    fn dt_text(n: u32) -> DataType {
        DataType::Text { size: Some(n) }
    }
    fn dt_bytes(n: u32) -> DataType {
        DataType::Bytes { size: Some(n) }
    }
    fn dt_dec(p: u32, s: u32) -> DataType {
        DataType::Decimal {
            precision: Some(p),
            scale: Some(s),
        }
    }
    const DT_BIGINT: DataType = DataType::BigInt { width: None };

    // §6.1 Identity & widening (truncate is no-op)

    // Identity / widening anchors live below as property tests
    // (`dispatch_identity_when_src_eq_dst`, `dispatch_text_to_text_size_widen`,
    // `dispatch_int_widen_preserves_value`). The plain-`#[test]` cases here
    // pin boundary semantics that the properties don't probe directly:
    // structured-payload truncate forbiddance, default substitution, exact
    // narrowing saturation, etc.

    #[test]
    fn json_to_json_truncate_forbidden() {
        let v = serde_json::json!({"a": 1});
        let res = convert(
            Value::Json(v),
            &DataType::Json,
            &DataType::Json,
            &truncate_ctx(),
        );
        assert!(matches!(res, Err(ConvertError::TruncationForbidden { .. })));
    }

    #[test]
    fn xml_to_xml_truncate_forbidden() {
        let res = convert(
            Value::Text("<a/>".into()),
            &DataType::Xml,
            &DataType::Xml,
            &truncate_ctx(),
        );
        assert!(matches!(res, Err(ConvertError::TruncationForbidden { .. })));
    }

    // §6.2 Null & default

    #[test]
    fn null_passthrough_no_default() {
        let out = convert(
            Value::Null,
            &DataType::Int32,
            &DataType::Int32,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Null);
    }

    #[test]
    fn default_substitutes_null() {
        let ctx = passthrough().with_default(Value::Int32(0));
        let out = convert(Value::Null, &DataType::Int32, &DataType::Int32, &ctx).unwrap();
        assert_eq!(out, Value::Int32(0));
    }

    #[test]
    fn default_ignored_when_value_present() {
        let ctx = passthrough().with_default(Value::Int32(99));
        let out = convert(Value::Int32(5), &DataType::Int32, &DataType::Int32, &ctx).unwrap();
        assert_eq!(out, Value::Int32(5));
    }

    // §6.3 Text narrowing (UTF-8 boundary)

    #[test]
    fn text_narrow_rejected_without_truncate() {
        let res = convert(
            Value::Text("hello".into()),
            &dt_text(20),
            &dt_text(10),
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn text_narrow_cuts_at_byte() {
        let out = convert(
            Value::Text("01234567890".into()),
            &dt_text(20),
            &dt_text(10),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("0123456789".into()));
    }

    #[test]
    fn text_narrow_counts_chars_not_bytes() {
        // "Привет" = 6 chars, max=5 chars → "Приве".
        let out = convert(
            Value::Text("Привет".into()),
            &dt_text(20),
            &dt_text(5),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("Приве".into()));
    }

    #[test]
    fn text_narrow_emoji_counts_as_one_char() {
        // 1 emoji char fits a max=3 chars sink — passthrough.
        let out = convert(
            Value::Text("😀".into()),
            &dt_text(20),
            &dt_text(3),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("😀".into()));
    }

    // §6.4 Bytes narrowing

    #[test]
    fn bytes_narrow_rejected_without_truncate() {
        let res = convert(
            Value::Bytes(vec![1; 15]),
            &dt_bytes(20),
            &dt_bytes(10),
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn bytes_narrow_cuts() {
        let out = convert(
            Value::Bytes(vec![1; 15]),
            &dt_bytes(20),
            &dt_bytes(10),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1; 10]));
    }

    // §6.5 Integer narrowing — saturating

    #[test]
    fn int64_to_int32_saturate_overflow() {
        let out = convert(
            Value::Int64(i32::MAX as i64 + 1),
            &DataType::Int64,
            &DataType::Int32,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Int32(i32::MAX));
    }

    #[test]
    fn int64_to_int16_saturate_underflow() {
        let out = convert(
            Value::Int64(i16::MIN as i64 - 1),
            &DataType::Int64,
            &DataType::Int16,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Int16(i16::MIN));
    }

    #[test]
    fn signed_to_unsigned_negative_to_zero() {
        let out = convert(
            Value::Int32(-1),
            &DataType::Int32,
            &DataType::UInt32,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::UInt32(0));
    }

    #[test]
    fn signed_to_unsigned_rejected_without_truncate() {
        let res = convert(
            Value::Int32(5),
            &DataType::Int32,
            &DataType::UInt32,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn unsigned_to_signed_saturate() {
        let out = convert(
            Value::UInt32(u32::MAX),
            &DataType::UInt32,
            &DataType::Int32,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Int32(i32::MAX));
    }

    // §6.6 Float narrowing — saturating

    #[test]
    fn f64_to_f32_saturate() {
        let out = convert(
            Value::Float64(f64::MAX),
            &DataType::Float64,
            &DataType::Float32,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Float32(f32::MAX));
    }

    #[test]
    fn f64_to_int_truncates_toward_zero() {
        let out = convert(
            Value::Float64(1.7),
            &DataType::Float64,
            &DataType::Int64,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Int64(1));
        let out = convert(
            Value::Float64(-1.7),
            &DataType::Float64,
            &DataType::Int64,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Int64(-1));
    }

    #[test]
    fn f64_nan_to_int_overflow() {
        let res = convert(
            Value::Float64(f64::NAN),
            &DataType::Float64,
            &DataType::Int64,
            &truncate_ctx(),
        );
        assert!(matches!(res, Err(ConvertError::Overflow { .. })));
    }

    // §6.7 BigInt / Decimal narrowing

    #[test]
    fn bigint_narrow_overflow_saturates_width() {
        use num_bigint::BigInt;
        use std::str::FromStr;
        let big = BigInt::from_str("99999999999").unwrap();
        let out = convert(
            Value::BigInt(big),
            &DataType::BigInt { width: Some(20) },
            &DataType::BigInt { width: Some(10) },
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::BigInt(BigInt::from_str("9999999999").unwrap()));
    }

    #[test]
    fn decimal_to_bigint_truncates_toward_zero() {
        let d: BigDecimal = "12.99".parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dt_dec(10, 2),
            &DT_BIGINT,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::BigInt(num_bigint::BigInt::from(12)));
    }

    #[test]
    fn decimal_to_int_overflow_saturates() {
        let d: BigDecimal = "9999999999.00".parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dt_dec(20, 2),
            &DataType::Int32,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Int32(i32::MAX));
    }

    // §6.7b Decimal → Decimal value-aware dispatch
    //
    // Mongo Decimal128 maps to canonical `Decimal { None, None }`, which the
    // static matrix can't compare against a bounded pg `numeric(12, 2)`.
    // The dispatcher now inspects the actual `BigDecimal` payload: if it
    // fits the target precision/scale, the cast is lossless and proceeds
    // without `truncate=true`; otherwise it falls back to the truncate
    // gate.
    const DEC_UNB: DataType = DataType::Decimal {
        precision: None,
        scale: None,
    };

    #[test]
    fn decimal_unbounded_to_bounded_fits_without_truncate() {
        // The motivating case: mongo Decimal128 carrying 123.45 routed
        // into pg `numeric(12, 2)` without the user setting truncate.
        let d: BigDecimal = "123.45".parse().unwrap();
        let out = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(12, 2), &passthrough()).unwrap();
        assert_eq!(out, Value::Decimal("123.45".parse().unwrap()));
    }

    #[test]
    fn decimal_widening_with_more_precision_and_scale_is_noop() {
        let d: BigDecimal = "12.34".parse().unwrap();
        let out = convert(
            Value::Decimal(d.clone()),
            &dt_dec(6, 2),
            &dt_dec(10, 4),
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Decimal(d));
    }

    #[test]
    fn decimal_narrowing_scale_rejected_without_truncate() {
        // 12.345 → Decimal(p, 2): scale shrinks from 3 to 2 → needs consent.
        let d: BigDecimal = "12.345".parse().unwrap();
        let res = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(10, 2), &passthrough());
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn decimal_narrowing_scale_with_truncate_truncates_toward_zero() {
        // RoundingMode::Down (truncate toward zero), shared with the
        // existing decimal_narrow path. 12.345 → 12.34 (not 12.35).
        let d: BigDecimal = "12.345".parse().unwrap();
        let out = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(10, 2), &truncate_ctx()).unwrap();
        assert_eq!(out, Value::Decimal("12.34".parse().unwrap()));
    }

    #[test]
    fn decimal_integer_overflow_rejected_without_truncate() {
        // 123456.78 → Decimal(4, 2): integer-digit capacity is p−s = 2,
        // value has 6 integer digits. No consent → error.
        let d: BigDecimal = "123456.78".parse().unwrap();
        let res = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(4, 2), &passthrough());
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn decimal_unbounded_to_unbounded_is_identity() {
        let d: BigDecimal = "9999999999999999.123456789".parse().unwrap();
        let out = convert(
            Value::Decimal(d.clone()),
            &DEC_UNB,
            &DEC_UNB,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Decimal(d));
    }

    #[test]
    fn decimal_unbounded_to_bounded_aligns_scale() {
        // Value 1.5 (scale 1) lands in Decimal(12, 2) — output mantissa
        // shape is normalised to scale 2 so sink bindings see the
        // expected precision/scale on the wire. Numerically equivalent.
        let d: BigDecimal = "1.5".parse().unwrap();
        let out = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(12, 2), &passthrough()).unwrap();
        assert_eq!(out, Value::Decimal("1.50".parse().unwrap()));
    }

    #[test]
    fn decimal_zero_into_bounded_fits() {
        // Zero has 0 integer digits and 0 fractional digits — always fits.
        let d: BigDecimal = "0".parse().unwrap();
        let out = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(4, 2), &passthrough()).unwrap();
        assert_eq!(out, Value::Decimal("0.00".parse().unwrap()));
    }

    #[test]
    fn decimal_negative_value_fits() {
        // Sign must not count toward the integer-digit budget.
        let d: BigDecimal = "-12.34".parse().unwrap();
        let out = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(4, 2), &passthrough()).unwrap();
        assert_eq!(out, Value::Decimal("-12.34".parse().unwrap()));
    }

    #[test]
    fn decimal_trailing_zeros_count_as_no_extra_scale() {
        // 12.30 normalised has scale 1; target scale 1 fits without
        // truncate even though the literal carried two fractional chars.
        let d: BigDecimal = "12.30".parse().unwrap();
        let out = convert(Value::Decimal(d), &DEC_UNB, &dt_dec(4, 1), &passthrough()).unwrap();
        assert_eq!(out, Value::Decimal("12.3".parse().unwrap()));
    }

    // §6.7c Decimal → Float64 / Float32 (truncate-tolerant)
    //
    // Motivating scenario: pg `NUMERIC(12, 2)` → QuestDB `DOUBLE`
    // (canonical `Decimal{12,2}` → `Float64`). `numeric(12, 2)` carries
    // ≤15 significant digits so the round-trip is lossless and the
    // dispatcher resolves without `truncate=true`. Oversize Decimals
    // (e.g. > f64::MAX) require `truncate=true` to saturate to ±∞;
    // otherwise the dispatcher surfaces `Overflow`.

    #[test]
    fn decimal_12_2_to_f64_happy_path() {
        let d: BigDecimal = "12345.67".parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dt_dec(12, 2),
            &DataType::Float64,
            &passthrough(),
        )
        .unwrap();
        match out {
            Value::Float64(f) => assert!((f - 12345.67).abs() < 1e-9),
            _ => panic!("expected Float64"),
        }
    }

    #[test]
    fn decimal_oversize_to_f64_rejects_without_truncate() {
        let mut digits = String::from("1");
        for _ in 0..350 {
            digits.push('0');
        }
        let d: BigDecimal = digits.parse().unwrap();
        let res = convert(
            Value::Decimal(d),
            &dt_dec(38, 0),
            &DataType::Float64,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Overflow { .. })));
    }

    #[test]
    fn decimal_oversize_to_f64_saturates_under_truncate() {
        let mut digits = String::from("1");
        for _ in 0..350 {
            digits.push('0');
        }
        let d: BigDecimal = digits.parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dt_dec(38, 0),
            &DataType::Float64,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Float64(f64::INFINITY));
    }

    #[test]
    fn decimal_7_2_to_f32_happy_path() {
        let d: BigDecimal = "1234.56".parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dt_dec(7, 2),
            &DataType::Float32,
            &passthrough(),
        )
        .unwrap();
        match out {
            Value::Float32(f) => assert!((f - 1234.56_f32).abs() < 1e-2),
            _ => panic!("expected Float32"),
        }
    }

    #[test]
    fn decimal_oversize_to_f32_rejects_without_truncate() {
        let mut digits = String::from("1");
        for _ in 0..50 {
            digits.push('0');
        }
        let d: BigDecimal = digits.parse().unwrap();
        let res = convert(
            Value::Decimal(d),
            &dt_dec(38, 0),
            &DataType::Float32,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Overflow { .. })));
    }

    #[test]
    fn decimal_oversize_to_f32_saturates_under_truncate() {
        let mut digits = String::from("1");
        for _ in 0..50 {
            digits.push('0');
        }
        let d: BigDecimal = digits.parse().unwrap();
        let out = convert(
            Value::Decimal(d),
            &dt_dec(38, 0),
            &DataType::Float32,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Float32(f32::INFINITY));
    }

    // §6.8 Json / Xml conversions

    #[test]
    fn json_to_text_serialize_truncate() {
        let v = serde_json::json!({"a": 1});
        let out = convert(
            Value::Json(v),
            &DataType::Json,
            &dt_text(5),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("{\"a\":".into()));
    }

    #[test]
    fn json_to_unbounded_text_no_truncate_needed() {
        let v = serde_json::json!({"a": 1});
        let out = convert(
            Value::Json(v),
            &DataType::Json,
            &DataType::Text { size: None },
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("{\"a\":1}".into()));
    }

    #[test]
    fn xml_to_text_unbounded_no_truncate_needed() {
        let out = convert(
            Value::Text("<a/>".into()),
            &DataType::Xml,
            &DataType::Text { size: None },
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("<a/>".into()));
    }

    #[test]
    fn text_to_xml_well_formed() {
        let out = convert(
            Value::Text("<a/>".into()),
            &dt_text(36),
            &DataType::Xml,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("<a/>".into()));
    }

    #[test]
    fn text_to_xml_malformed_rejected() {
        let res = convert(
            Value::Text("<a>".into()),
            &dt_text(36),
            &DataType::Xml,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::InvalidXml { .. })));
    }

    // §6.9 Timestamp/Date

    #[test]
    fn timestamp_to_date_with_truncate() {
        use chrono::{DateTime, Utc};
        let ts: DateTime<Utc> = "2024-01-15T18:00:00Z".parse().unwrap();
        let out = convert(
            Value::Timestamp(ts),
            &DataType::Timestamp,
            &DataType::Date,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Date(ts.date_naive()));
    }

    #[test]
    fn timestamp_to_date_rejected_without_truncate() {
        use chrono::{DateTime, Utc};
        let ts: DateTime<Utc> = "2024-01-15T18:00:00Z".parse().unwrap();
        let res = convert(
            Value::Timestamp(ts),
            &DataType::Timestamp,
            &DataType::Date,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    // §6.9b Text → Bool

    #[test]
    fn text_to_bool_truthy() {
        for s in ["y", "Y", "t", "1", "true", "TRUE", "yes", "YES"] {
            let out = convert(
                Value::Text(s.into()),
                &dt_text(10),
                &DataType::Bool,
                &passthrough(),
            )
            .unwrap();
            assert_eq!(out, Value::Bool(true), "input {s:?}");
        }
    }

    #[test]
    fn text_to_bool_falsy() {
        for s in ["n", "N", "f", "0", "false", "no"] {
            let out = convert(
                Value::Text(s.into()),
                &dt_text(10),
                &DataType::Bool,
                &passthrough(),
            )
            .unwrap();
            assert_eq!(out, Value::Bool(false), "input {s:?}");
        }
    }

    #[test]
    fn text_to_bool_invalid() {
        for s in ["maybe", "", "2", " yes "] {
            let res = convert(
                Value::Text(s.into()),
                &dt_text(10),
                &DataType::Bool,
                &passthrough(),
            );
            assert!(
                matches!(res, Err(ConvertError::InvalidBool { .. })),
                "{s:?}"
            );
        }
    }

    // Existing UUID round-trip regression coverage.

    #[test]
    fn value_shape_mismatch_on_each_dispatcher_arm() {
        // For each (src, dst), pass a deliberately-wrong-variant value and
        // assert ValueShapeMismatch is surfaced by the dispatcher arm.
        let wrong_bool = Value::Bool(true);
        let wrong_int = Value::Int32(1);
        let bigint_unbounded = DataType::BigInt { width: None };
        let dec = DataType::Decimal {
            precision: Some(20),
            scale: Some(0),
        };

        let cases: Vec<(DataType, DataType, Value)> = vec![
            // UUID round-trips
            (DataType::Uuid, dt_text(36), wrong_bool.clone()),
            (DataType::Uuid, dt_bytes(16), wrong_bool.clone()),
            (dt_text(36), DataType::Uuid, wrong_bool.clone()),
            (dt_bytes(16), DataType::Uuid, wrong_bool.clone()),
            // Int → Bool (wrong variant: Bool, expected Int*)
            (DataType::Int16, DataType::Bool, wrong_bool.clone()),
            (DataType::Int32, DataType::Bool, wrong_bool.clone()),
            (DataType::Int64, DataType::Bool, wrong_bool.clone()),
            // UInt → Bool
            (DataType::UInt8, DataType::Bool, wrong_bool.clone()),
            (DataType::UInt16, DataType::Bool, wrong_bool.clone()),
            (DataType::UInt32, DataType::Bool, wrong_bool.clone()),
            (DataType::UInt64, DataType::Bool, wrong_bool.clone()),
            // Bool → Int*/UInt*
            (DataType::Bool, DataType::Int16, wrong_int.clone()),
            (DataType::Bool, DataType::Int32, wrong_int.clone()),
            (DataType::Bool, DataType::Int64, wrong_int.clone()),
            (DataType::Bool, DataType::UInt8, wrong_int.clone()),
            (DataType::Bool, DataType::UInt16, wrong_int.clone()),
            (DataType::Bool, DataType::UInt32, wrong_int.clone()),
            (DataType::Bool, DataType::UInt64, wrong_int.clone()),
            // Int widening
            (DataType::Int16, DataType::Int32, wrong_bool.clone()),
            (DataType::Int16, DataType::Int64, wrong_bool.clone()),
            (DataType::Int32, DataType::Int64, wrong_bool.clone()),
            (DataType::Int16, DataType::Float32, wrong_bool.clone()),
            (DataType::Int16, DataType::Float64, wrong_bool.clone()),
            (DataType::Int32, DataType::Float64, wrong_bool.clone()),
            (DataType::Float32, DataType::Float64, wrong_bool.clone()),
            // UInt widening (within unsigned and to signed)
            (DataType::UInt8, DataType::UInt16, wrong_bool.clone()),
            (DataType::UInt8, DataType::UInt32, wrong_bool.clone()),
            (DataType::UInt8, DataType::UInt64, wrong_bool.clone()),
            (DataType::UInt16, DataType::UInt32, wrong_bool.clone()),
            (DataType::UInt16, DataType::UInt64, wrong_bool.clone()),
            (DataType::UInt32, DataType::UInt64, wrong_bool.clone()),
            (DataType::UInt8, DataType::Int16, wrong_bool.clone()),
            (DataType::UInt8, DataType::Int32, wrong_bool.clone()),
            (DataType::UInt8, DataType::Int64, wrong_bool.clone()),
            (DataType::UInt16, DataType::Int32, wrong_bool.clone()),
            (DataType::UInt16, DataType::Int64, wrong_bool.clone()),
            (DataType::UInt32, DataType::Int64, wrong_bool.clone()),
            // Int → BigInt
            (
                DataType::Int16,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            (
                DataType::Int32,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            (
                DataType::Int64,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            // UInt → BigInt
            (
                DataType::UInt8,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            (
                DataType::UInt16,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            (
                DataType::UInt32,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            (
                DataType::UInt64,
                bigint_unbounded.clone(),
                wrong_bool.clone(),
            ),
            // Int → Decimal
            (DataType::Int16, dec.clone(), wrong_bool.clone()),
            (DataType::Int32, dec.clone(), wrong_bool.clone()),
            (DataType::Int64, dec.clone(), wrong_bool.clone()),
            // UInt → Decimal
            (DataType::UInt8, dec.clone(), wrong_bool.clone()),
            (DataType::UInt16, dec.clone(), wrong_bool.clone()),
            (DataType::UInt32, dec.clone(), wrong_bool.clone()),
            (DataType::UInt64, dec.clone(), wrong_bool.clone()),
            // BigInt → Decimal
            (bigint_unbounded, dec.clone(), wrong_bool.clone()),
        ];

        for (src, dst, wrong_value) in cases {
            let res = convert(wrong_value.clone(), &src, &dst, &passthrough());
            assert!(
                matches!(res, Err(ConvertError::ValueShapeMismatch { .. })),
                "expected ValueShapeMismatch for ({src:?} -> {dst:?}) with {wrong_value:?}, got {res:?}"
            );
        }
    }

    #[test]
    fn json_to_text_value_shape_mismatch() {
        let res = convert(
            Value::Int32(1),
            &DataType::Json,
            &DataType::Text { size: None },
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn xml_to_text_value_shape_mismatch() {
        let res = convert(
            Value::Int32(1),
            &DataType::Xml,
            &DataType::Text { size: None },
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn text_to_bool_value_shape_mismatch() {
        let res = convert(
            Value::Int32(1),
            &dt_text(10),
            &DataType::Bool,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn json_to_json_identity_passthrough() {
        let v = serde_json::json!({"a": 1});
        let out = convert(
            Value::Json(v.clone()),
            &DataType::Json,
            &DataType::Json,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Json(v));
    }

    #[test]
    fn unsupported_pair_with_no_arm() {
        // `Date → Uuid` has no defined conversion in either matrix.
        let res = convert(
            Value::Date(chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            &DataType::Date,
            &DataType::Uuid,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    // ---- Bool ↔ Float / BigInt / Decimal --------------------------

    #[test]
    fn bool_to_float32_lossless() {
        let out = convert(
            Value::Bool(true),
            &DataType::Bool,
            &DataType::Float32,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Float32(1.0));
        let out = convert(
            Value::Bool(false),
            &DataType::Bool,
            &DataType::Float32,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Float32(0.0));
    }

    #[test]
    fn bool_to_float64_lossless() {
        let out = convert(
            Value::Bool(true),
            &DataType::Bool,
            &DataType::Float64,
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Float64(1.0));
    }

    #[test]
    fn bool_to_bigint_lossless() {
        let out = convert(
            Value::Bool(true),
            &DataType::Bool,
            &DataType::BigInt { width: None },
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::BigInt(BigInt::from(1u8)));
        let out = convert(
            Value::Bool(false),
            &DataType::Bool,
            &DataType::BigInt { width: Some(4) },
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::BigInt(BigInt::from(0u8)));
    }

    #[test]
    fn bool_to_decimal_lossless() {
        let out = convert(
            Value::Bool(true),
            &DataType::Bool,
            &DataType::Decimal {
                precision: Some(5),
                scale: Some(2),
            },
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Decimal(BigDecimal::from(1u8)));
    }

    #[test]
    fn bigint_to_bool_truncate_zero_vs_nonzero() {
        let zero = convert(
            Value::BigInt(BigInt::from(0)),
            &DataType::BigInt { width: None },
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(zero, Value::Bool(false));
        let huge = convert(
            Value::BigInt(BigInt::from(1_234_567_890_i64).pow(3)),
            &DataType::BigInt { width: None },
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(huge, Value::Bool(true));
    }

    #[test]
    fn bigint_to_bool_without_truncate_unsupported() {
        let res = convert(
            Value::BigInt(BigInt::from(7)),
            &DataType::BigInt { width: None },
            &DataType::Bool,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn decimal_to_bool_truncate_zero_vs_nonzero() {
        let zero = convert(
            Value::Decimal("0.0".parse().unwrap()),
            &DataType::Decimal {
                precision: Some(5),
                scale: Some(2),
            },
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(zero, Value::Bool(false));
        let neg = convert(
            Value::Decimal("-0.01".parse().unwrap()),
            &DataType::Decimal {
                precision: Some(5),
                scale: Some(2),
            },
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(neg, Value::Bool(true));
    }

    #[test]
    fn float_to_bool_truncate_positive_zero_is_false() {
        let out = convert(
            Value::Float64(0.0),
            &DataType::Float64,
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Bool(false));
    }

    /// IEEE-754 `-0.0 == 0.0` under the equality operator, so the
    /// `f != 0.0` runtime check returns `false` for both signed zeros.
    #[test]
    fn float_to_bool_truncate_negative_zero_is_false() {
        let out = convert(
            Value::Float64(-0.0),
            &DataType::Float64,
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Bool(false));
    }

    #[test]
    fn float_to_bool_truncate_infinities_are_true() {
        let pos = convert(
            Value::Float64(f64::INFINITY),
            &DataType::Float64,
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(pos, Value::Bool(true));
        let neg = convert(
            Value::Float64(f64::NEG_INFINITY),
            &DataType::Float64,
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(neg, Value::Bool(true));
    }

    #[test]
    fn float_to_bool_truncate_nan_returns_default_when_set() {
        let mut ctx = truncate_ctx();
        ctx.default = Some(Value::Bool(false));
        let out = convert(
            Value::Float64(f64::NAN),
            &DataType::Float64,
            &DataType::Bool,
            &ctx,
        )
        .unwrap();
        assert_eq!(out, Value::Bool(false));
    }

    #[test]
    fn float_to_bool_truncate_nan_returns_null_without_default() {
        // No default configured. Validation is expected to require a
        // default when the sink column is non-nullable; the runtime
        // defensively emits `Value::Null`.
        let out = convert(
            Value::Float64(f64::NAN),
            &DataType::Float64,
            &DataType::Bool,
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Null);
    }

    #[test]
    fn float_to_bool_without_truncate_unsupported() {
        let res = convert(
            Value::Float64(1.0),
            &DataType::Float64,
            &DataType::Bool,
            &passthrough(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn uuid_to_text_canonical() {
        let u = UuidVal::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let out = convert(
            Value::Uuid(u),
            &DataType::Uuid,
            &dt_text(36),
            &passthrough(),
        )
        .unwrap();
        assert_eq!(
            out,
            Value::Text("550e8400-e29b-41d4-a716-446655440000".into())
        );
    }

    // ---- Union source — runtime per-value re-dispatch -------------

    #[test]
    fn union_src_picks_int32_arm_at_runtime() {
        // Use a genuinely heterogeneous Union — Int32 + UInt32 don't
        // collapse via widening rules (signed/unsigned families stay
        // separate in `collapse_union`), so we get a real
        // `DataType::Union(Int32 | UInt32)` rather than a folded leaf.
        // A runtime Int32 value must be re-dispatched as Int32 → Int64.
        let src = DataType::union(vec![DataType::Int32, DataType::UInt32]);
        assert!(matches!(src, DataType::Union(_)));
        let out = convert(Value::Int32(42), &src, &DataType::Int64, &passthrough()).unwrap();
        assert_eq!(out, Value::Int64(42));
    }

    #[test]
    fn union_src_picks_int64_arm_at_runtime() {
        // Same heterogeneous union — runtime UInt32 value must be
        // re-dispatched as UInt32 → Int64 (lossless widening).
        let src = DataType::union(vec![DataType::Int32, DataType::UInt32]);
        let out = convert(Value::UInt32(99), &src, &DataType::Int64, &passthrough()).unwrap();
        assert_eq!(out, Value::Int64(99));
    }

    #[test]
    fn union_src_null_returns_null() {
        // Null short-circuits before the union-dispatch arm; no default
        // means we propagate Value::Null untouched.
        let src = DataType::union(vec![DataType::Int32, DataType::Int64]);
        let out = convert(Value::Null, &src, &DataType::Int64, &passthrough()).unwrap();
        assert_eq!(out, Value::Null);
    }

    // ---- Custom routing ------------------------------------------

    use crate::dynamic::{DynType, DynValue};
    use std::any::Any;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CONVERT_CALLS: AtomicUsize = AtomicUsize::new(0);

    #[derive(Debug)]
    struct DispatchTestType;

    impl DynType for DispatchTestType {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.dispatch"
        }
        fn can_convert_to(&self, target: &DataType, _trunc: bool) -> bool {
            matches!(target, DataType::Bytes { size: None })
        }
        fn can_construct_from(&self, src: &DataType, _trunc: bool) -> bool {
            matches!(src, DataType::Bytes { size: None })
        }
        fn convert(
            &self,
            v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            CONVERT_CALLS.fetch_add(1, Ordering::SeqCst);
            // Translate the opaque value's payload back to bytes so the
            // caller sees a deterministic result.
            match v {
                Value::Custom(inner) => {
                    let v = inner
                        .as_any()
                        .downcast_ref::<DispatchTestValue>()
                        .map(|v| v.0.clone())
                        .unwrap_or_default();
                    Ok(Value::Bytes(v))
                }
                _ => Err(ConvertError::ValueShapeMismatch {
                    src: DataType::Custom(Box::new(DispatchTestType)),
                }),
            }
        }
        fn construct(
            &self,
            v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            match v {
                Value::Bytes(b) => Ok(Value::Custom(Box::new(DispatchTestValue(b)))),
                _ => Err(ConvertError::ValueShapeMismatch {
                    src: DataType::Bytes { size: None },
                }),
            }
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(DispatchTestType)
        }
    }

    #[derive(Debug, Clone)]
    struct DispatchTestValue(Vec<u8>);

    impl DynValue for DispatchTestValue {
        fn dyn_type(&self) -> Box<dyn DynType> {
            Box::new(DispatchTestType)
        }
        fn as_any(&self) -> &dyn Any {
            self
        }
        fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
            self
        }
        fn is_equal(&self, other: &dyn DynValue) -> bool {
            other
                .as_any()
                .downcast_ref::<DispatchTestValue>()
                .map(|o| o.0 == self.0)
                .unwrap_or(false)
        }
        fn clone_box(&self) -> Box<dyn DynValue> {
            Box::new(self.clone())
        }
    }

    #[test]
    fn convert_custom_to_bytes_invokes_trait() {
        let before = CONVERT_CALLS.load(Ordering::SeqCst);
        let v = Value::Custom(Box::new(DispatchTestValue(vec![1, 2, 3])));
        let out = convert(
            v,
            &DataType::Custom(Box::new(DispatchTestType)),
            &DataType::Bytes { size: None },
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3]));
        assert_eq!(CONVERT_CALLS.load(Ordering::SeqCst), before + 1);
    }

    #[test]
    fn convert_bytes_to_custom_via_construct() {
        let out = convert(
            Value::Bytes(vec![9, 9]),
            &DataType::Bytes { size: None },
            &DataType::Custom(Box::new(DispatchTestType)),
            &passthrough(),
        )
        .unwrap();
        match out {
            Value::Custom(v) => {
                let inner = v
                    .as_any()
                    .downcast_ref::<DispatchTestValue>()
                    .expect("downcast");
                assert_eq!(inner.0, vec![9, 9]);
            }
            other => panic!("expected Value::Custom, got {other:?}"),
        }
    }

    #[test]
    fn convert_custom_to_custom_identity_passthrough() {
        let v = Value::Custom(Box::new(DispatchTestValue(vec![5])));
        let out = convert(
            v.clone(),
            &DataType::Custom(Box::new(DispatchTestType)),
            &DataType::Custom(Box::new(DispatchTestType)),
            &passthrough(),
        )
        .unwrap();
        assert_eq!(out, v);
    }

    // ---- Property-based dispatcher invariants -----------------------
    //
    // These properties lock the high-level routing rules of `convert`
    // without forcing per-cell exhaustive enumeration. The strategies
    // deliberately stay narrow: `prop_oneof!` over a fixed set of
    // (DataType, Value) concrete shapes rather than encoding the full
    // matching relation generically. Custom / Union are excluded — those
    // arms are exercised by the named tests above.

    use proptest::prelude::*;

    /// Yields a `(DataType, Value)` pair where the value variant matches
    /// the declared type. Covers every non-Custom, non-Union simple
    /// variant: numerics, Bool, Text/Bytes (sized + unbounded), Date,
    /// Timestamp, Uuid, Ipv4/Ipv6, Json, Xml (Xml carries a
    /// `Value::Text`), BigInt and Decimal (bounded + unbounded).
    fn matching_type_and_value() -> impl Strategy<Value = (DataType, Value)> {
        (0u32..24).prop_flat_map(matching_arm)
    }

    fn matching_arm(choice: u32) -> BoxedStrategy<(DataType, Value)> {
        match choice {
            0 => any::<bool>()
                .prop_map(|b| (DataType::Bool, Value::Bool(b)))
                .boxed(),
            1 => any::<i8>()
                .prop_map(|n| (DataType::Int8, Value::Int8(n)))
                .boxed(),
            2 => any::<i16>()
                .prop_map(|n| (DataType::Int16, Value::Int16(n)))
                .boxed(),
            3 => any::<i32>()
                .prop_map(|n| (DataType::Int32, Value::Int32(n)))
                .boxed(),
            4 => any::<i64>()
                .prop_map(|n| (DataType::Int64, Value::Int64(n)))
                .boxed(),
            5 => any::<u8>()
                .prop_map(|n| (DataType::UInt8, Value::UInt8(n)))
                .boxed(),
            6 => any::<u16>()
                .prop_map(|n| (DataType::UInt16, Value::UInt16(n)))
                .boxed(),
            7 => any::<u32>()
                .prop_map(|n| (DataType::UInt32, Value::UInt32(n)))
                .boxed(),
            8 => any::<u64>()
                .prop_map(|n| (DataType::UInt64, Value::UInt64(n)))
                .boxed(),
            // Finite floats only — NaN equality breaks the identity assert
            // (NaN != NaN), and the dispatcher's identity arm just hands
            // the value back regardless. Float NaN routing is pinned by
            // dedicated tests above.
            9 => proptest::num::f32::NORMAL
                .prop_map(|f| (DataType::Float32, Value::Float32(f)))
                .boxed(),
            10 => proptest::num::f64::NORMAL
                .prop_map(|f| (DataType::Float64, Value::Float64(f)))
                .boxed(),
            11 => "[a-zA-Z0-9 ]{0,32}"
                .prop_map(|s: String| {
                    (
                        DataType::Text {
                            size: Some(s.chars().count() as u32 + 8),
                        },
                        Value::Text(s),
                    )
                })
                .boxed(),
            12 => "[a-zA-Z0-9 ]{0,32}"
                .prop_map(|s: String| (DataType::Text { size: None }, Value::Text(s)))
                .boxed(),
            13 => proptest::collection::vec(any::<u8>(), 0..32)
                .prop_map(|b| {
                    (
                        DataType::Bytes {
                            size: Some(b.len() as u32 + 4),
                        },
                        Value::Bytes(b),
                    )
                })
                .boxed(),
            14 => proptest::collection::vec(any::<u8>(), 0..32)
                .prop_map(|b| (DataType::Bytes { size: None }, Value::Bytes(b)))
                .boxed(),
            15 => (-30_000i64..30_000)
                .prop_map(|days| {
                    let d = chrono::NaiveDate::from_ymd_opt(1970, 1, 1)
                        .unwrap()
                        .checked_add_signed(chrono::Duration::days(days))
                        .unwrap();
                    (DataType::Date, Value::Date(d))
                })
                .boxed(),
            16 => (-30_000i64..30_000, 0i64..86_400)
                .prop_map(|(days, secs)| {
                    let dt =
                        chrono::DateTime::<chrono::Utc>::from_timestamp(days * 86_400 + secs, 0)
                            .unwrap();
                    (DataType::Timestamp, Value::Timestamp(dt))
                })
                .boxed(),
            17 => any::<[u8; 16]>()
                .prop_map(|b| (DataType::Uuid, Value::Uuid(::uuid::Uuid::from_bytes(b))))
                .boxed(),
            18 => any::<[u8; 4]>()
                .prop_map(|b| (DataType::Ipv4, Value::Ipv4(std::net::Ipv4Addr::from(b))))
                .boxed(),
            19 => any::<[u8; 16]>()
                .prop_map(|b| (DataType::Ipv6, Value::Ipv6(std::net::Ipv6Addr::from(b))))
                .boxed(),
            20 => any::<i64>()
                .prop_map(|n| {
                    (
                        DataType::Json,
                        Value::Json(serde_json::Value::Number(serde_json::Number::from(n))),
                    )
                })
                .boxed(),
            21 => prop_oneof![
                Just((DataType::Xml, Value::Text("<a/>".into()))),
                Just((DataType::Xml, Value::Text("<b></b>".into()))),
                Just((DataType::Xml, Value::Text("<root x=\"1\"/>".into()))),
                Just((DataType::Xml, Value::Text("<a><inner/></a>".into()))),
            ]
            .boxed(),
            22 => any::<i64>()
                .prop_map(|n| {
                    (
                        DataType::BigInt { width: None },
                        Value::BigInt(num_bigint::BigInt::from(n)),
                    )
                })
                .boxed(),
            _ => any::<i32>()
                .prop_map(|n| {
                    (
                        DataType::Decimal {
                            precision: None,
                            scale: None,
                        },
                        Value::Decimal(bigdecimal::BigDecimal::from(n)),
                    )
                })
                .boxed(),
        }
    }

    /// `convert(v, dt, dt, ctx)` is an identity for every matching
    /// `(DataType, Value)` pair — except for the structured-payload
    /// truncate-forbidden combinations (`Json/Xml` with `truncate=true`)
    /// which are pinned by dedicated `#[test]` cases above. Runs against
    /// both passthrough and truncate contexts so the truncate-noop
    /// behaviour for non-structured types is also covered.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn dispatch_identity_when_src_eq_dst(
        #[strategy(matching_type_and_value())] pair: (DataType, Value),
    ) {
        let (dt, v) = pair;
        let is_structured = matches!(dt, DataType::Json | DataType::Xml);

        // Passthrough — always identity.
        let out = convert(v.clone(), &dt, &dt, &passthrough());
        prop_assert!(
            out.is_ok(),
            "passthrough identity failed for {dt:?}: {out:?}"
        );
        prop_assert_eq!(
            out.unwrap(),
            v.clone(),
            "passthrough identity diverged for {:?}",
            dt
        );

        // Truncate — identity except for the structured payloads, which
        // must explicitly refuse rather than silently round-trip.
        let out_t = convert(v.clone(), &dt, &dt, &truncate_ctx());
        if is_structured {
            prop_assert!(
                matches!(out_t, Err(ConvertError::TruncationForbidden { .. })),
                "expected TruncationForbidden for {dt:?} with truncate, got {out_t:?}"
            );
        } else {
            prop_assert!(out_t.is_ok(), "truncate identity failed for {dt:?}");
            prop_assert_eq!(out_t.unwrap(), v, "truncate identity diverged for {:?}", dt);
        }
    }

    /// `Text { Some(n) } → Text { Some(m) }` with `m ≥ n` always
    /// succeeds and preserves the value. `Text { _ } → Text { None }`
    /// (unbounded sink) also always succeeds. Runs under both ctx
    /// flavours so the truncate-noop is exercised too.
    #[test_strategy::proptest(ProptestConfig::with_cases(128))]
    fn dispatch_text_to_text_size_widen(
        #[strategy(0u32..32)] src_size: u32,
        #[strategy(0u32..64)] grow: u32,
        #[strategy("[a-zA-Z0-9 ]{0,16}")] payload: String,
        #[strategy(any::<bool>())] use_truncate: bool,
        #[strategy(any::<bool>())] unbounded_sink: bool,
    ) {
        let src = DataType::Text {
            size: Some(src_size + 1),
        };
        let dst = if unbounded_sink {
            DataType::Text { size: None }
        } else {
            DataType::Text {
                size: Some(src_size + 1 + grow),
            }
        };
        let ctx = if use_truncate {
            truncate_ctx()
        } else {
            passthrough()
        };
        let out = convert(Value::Text(payload.clone()), &src, &dst, &ctx);
        prop_assert!(out.is_ok(), "widen {src:?} → {dst:?} failed: {out:?}");
        prop_assert_eq!(out.unwrap(), Value::Text(payload));
    }

    /// Widening between fixed-width signed integer pairs preserves the
    /// underlying i64 value. The strategy emits an `(src_dt, dst_dt,
    /// src_value)` triple where src ≤ dst in width and the value fits
    /// the source's signed range.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn dispatch_int_widen_preserves_value(
        #[strategy(int_widening_pair())] triple: (DataType, DataType, i64),
    ) {
        let (src, dst, n) = triple;
        let src_value = match &src {
            DataType::Int8 => Value::Int8(n as i8),
            DataType::Int16 => Value::Int16(n as i16),
            DataType::Int32 => Value::Int32(n as i32),
            DataType::Int64 => Value::Int64(n),
            _ => unreachable!(),
        };
        let out = convert(src_value, &src, &dst, &passthrough())
            .unwrap_or_else(|e| panic!("widening {src:?} → {dst:?} failed: {e:?}"));
        let widened: i64 = match out {
            Value::Int8(x) => i64::from(x),
            Value::Int16(x) => i64::from(x),
            Value::Int32(x) => i64::from(x),
            Value::Int64(x) => x,
            other => panic!("expected integer Value, got {other:?}"),
        };
        prop_assert_eq!(widened, n, "widening {:?} → {:?} changed value", src, dst);
    }

    /// Strategy yielding `(src, dst, value_in_src_range)` for signed
    /// integer widening pairs (src width ≤ dst width).
    fn int_widening_pair() -> impl Strategy<Value = (DataType, DataType, i64)> {
        let pairs: Vec<(DataType, DataType, i64, i64)> = vec![
            (
                DataType::Int8,
                DataType::Int8,
                i64::from(i8::MIN),
                i64::from(i8::MAX),
            ),
            (
                DataType::Int8,
                DataType::Int16,
                i64::from(i8::MIN),
                i64::from(i8::MAX),
            ),
            (
                DataType::Int8,
                DataType::Int32,
                i64::from(i8::MIN),
                i64::from(i8::MAX),
            ),
            (
                DataType::Int8,
                DataType::Int64,
                i64::from(i8::MIN),
                i64::from(i8::MAX),
            ),
            (
                DataType::Int16,
                DataType::Int16,
                i64::from(i16::MIN),
                i64::from(i16::MAX),
            ),
            (
                DataType::Int16,
                DataType::Int32,
                i64::from(i16::MIN),
                i64::from(i16::MAX),
            ),
            (
                DataType::Int16,
                DataType::Int64,
                i64::from(i16::MIN),
                i64::from(i16::MAX),
            ),
            (
                DataType::Int32,
                DataType::Int32,
                i64::from(i32::MIN),
                i64::from(i32::MAX),
            ),
            (
                DataType::Int32,
                DataType::Int64,
                i64::from(i32::MIN),
                i64::from(i32::MAX),
            ),
            (DataType::Int64, DataType::Int64, i64::MIN, i64::MAX),
        ];
        proptest::sample::select(pairs)
            .prop_flat_map(|(src, dst, lo, hi)| (Just(src), Just(dst), lo..=hi))
    }

    /// Known narrowing pair (`Int64 → Int8`) with an out-of-range value
    /// errors with `Unsupported` when `truncate=false`. Adding consent
    /// (`truncate=true`) flips it to a successful saturating cast — the
    /// property doesn't assert the saturated value (that is the job of
    /// the named anchors above), only that the dispatcher gates on
    /// `truncate`.
    #[test_strategy::proptest(ProptestConfig::with_cases(128))]
    fn dispatch_truncate_off_blocks_narrowing(
        #[strategy(prop_oneof![
            (i64::from(i8::MAX) + 1)..=i64::MAX,
            i64::MIN..=(i64::from(i8::MIN) - 1),
        ])]
        out_of_range: i64,
    ) {
        let res = convert(
            Value::Int64(out_of_range),
            &DataType::Int64,
            &DataType::Int8,
            &passthrough(),
        );
        prop_assert!(
            matches!(res, Err(ConvertError::Unsupported { .. })),
            "narrowing Int64({out_of_range}) → Int8 must require truncate, got {res:?}"
        );

        let ok = convert(
            Value::Int64(out_of_range),
            &DataType::Int64,
            &DataType::Int8,
            &truncate_ctx(),
        );
        prop_assert!(
            ok.is_ok(),
            "narrowing with truncate must succeed, got {ok:?}"
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod ip_dispatch_tests {
    use super::*;
    use crate::Value;

    fn pt() -> ConversionContext {
        ConversionContext::passthrough()
    }
    fn tr() -> ConversionContext {
        ConversionContext {
            default: None,
            truncate: true,
        }
    }

    #[test]
    fn ipv4_to_ipv6_lossless_widens_to_mapped() {
        let v = Value::Ipv4(std::net::Ipv4Addr::new(203, 0, 113, 42));
        let out = convert(v, &DataType::Ipv4, &DataType::Ipv6, &pt()).unwrap();
        let Value::Ipv6(a) = out else { panic!() };
        assert_eq!(a.to_string(), "::ffff:203.0.113.42");
    }

    #[test]
    fn ipv6_to_ipv4_requires_truncate() {
        let v = Value::Ipv6("::ffff:203.0.113.42".parse().unwrap());
        // Without truncate: rejected (Unsupported, since matrix admits
        // only under truncate; dispatcher mirrors).
        let res = convert(v.clone(), &DataType::Ipv6, &DataType::Ipv4, &pt());
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
        // With truncate + IPv4-mapped: succeeds.
        let out = convert(v, &DataType::Ipv6, &DataType::Ipv4, &tr()).unwrap();
        assert_eq!(out, Value::Ipv4(std::net::Ipv4Addr::new(203, 0, 113, 42)));
    }

    #[test]
    fn ipv6_to_ipv4_non_mapped_errors_at_runtime() {
        let v = Value::Ipv6("2001:db8::1".parse().unwrap());
        let res = convert(v, &DataType::Ipv6, &DataType::Ipv4, &tr());
        assert!(matches!(res, Err(ConvertError::IpV6NotMappable { .. })));
    }

    #[test]
    fn text_to_ipv4_and_back() {
        let v = Value::Text("192.0.2.1".into());
        let ipv4 = convert(v, &DataType::Text { size: None }, &DataType::Ipv4, &pt()).unwrap();
        assert_eq!(ipv4, Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 1)));
        let back = convert(ipv4, &DataType::Ipv4, &DataType::Text { size: None }, &pt()).unwrap();
        assert_eq!(back, Value::Text("192.0.2.1".into()));
    }

    #[test]
    fn text_to_ipv6_round_trips_canonical_form() {
        let v = Value::Text("2001:0db8:0000:0000:0000:0000:0000:0001".into());
        let ipv6 = convert(v, &DataType::Text { size: None }, &DataType::Ipv6, &pt()).unwrap();
        let back = convert(ipv6, &DataType::Ipv6, &DataType::Text { size: None }, &pt()).unwrap();
        // Canonical form (RFC 5952): "2001:db8::1".
        assert_eq!(back, Value::Text("2001:db8::1".into()));
    }

    #[test]
    fn ipv4_bytes_round_trip_network_order() {
        let v = Value::Ipv4(std::net::Ipv4Addr::new(10, 0, 0, 1));
        let bytes = convert(v, &DataType::Ipv4, &DataType::Bytes { size: None }, &pt()).unwrap();
        assert_eq!(bytes, Value::Bytes(vec![10, 0, 0, 1]));
        let back = convert(
            bytes,
            &DataType::Bytes { size: None },
            &DataType::Ipv4,
            &pt(),
        )
        .unwrap();
        assert_eq!(back, Value::Ipv4(std::net::Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[test]
    fn bytes_to_ipv4_wrong_length_errors() {
        let res = convert(
            Value::Bytes(vec![1, 2, 3]),
            &DataType::Bytes { size: None },
            &DataType::Ipv4,
            &pt(),
        );
        assert!(matches!(res, Err(ConvertError::Length { expected: 4, .. })));
    }

    #[test]
    fn text_to_ipv6_then_to_ipv4_via_mapped_path() {
        let v = Value::Text("::ffff:203.0.113.42".into());
        let ipv6 = convert(v, &DataType::Text { size: None }, &DataType::Ipv6, &pt()).unwrap();
        let ipv4 = convert(ipv6, &DataType::Ipv6, &DataType::Ipv4, &tr()).unwrap();
        assert_eq!(ipv4, Value::Ipv4(std::net::Ipv4Addr::new(203, 0, 113, 42)));
    }
}
