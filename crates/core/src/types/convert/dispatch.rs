//! `(src, dst, ctx)` dispatcher for value conversion. See module docs.
//!
//! Becomes a thin router: identity / pure-widening short-circuits stay
//! here; everything narrowing-or-cross-type is delegated to a per-group
//! submodule (`int_narrow`, `text_narrow`, `json_text`, `xml_text`, etc.).
//! Truncate-only paths require `ctx.truncate=true`; explicitly-forbidden
//! truncate combinations return [`ConvertError::TruncationForbidden`].

use super::ConvertError;
use super::context::ConversionContext;
use super::{
    bigint_narrow, bytes_narrow, decimal_narrow, float_narrow, int_narrow, json_text, text_bool,
    text_narrow, timestamp_date, uuid as uuid_conv, xml_text,
};
use crate::types::{DataType, Value};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;

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
            src: *src,
            dst: *dst,
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
            src: *src,
            dst: *dst,
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
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Uuid, DataType::Bytes { .. }) => match value {
            Value::Uuid(u) => Ok(Value::Bytes(uuid_conv::to_bytes(u).to_vec())),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Text { .. }, DataType::Uuid) => match value {
            Value::Text(s) => Ok(Value::Uuid(uuid_conv::parse_text(&s)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },
        (DataType::Bytes { .. }, DataType::Uuid) => match value {
            Value::Bytes(b) => Ok(Value::Uuid(uuid_conv::from_bytes(&b)?)),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // ---- Int / UInt ↔ Bool (existing) -----------------------------
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

        // ---- Text → Bool lexer (always allowed, no truncate gate) ------
        (DataType::Text { .. }, DataType::Bool) => text_bool::convert(value, src),

        // ---- Numeric widening (existing) ------------------------------
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

        // Float narrowing — only with truncate.
        (DataType::Float64, DataType::Float32)
        | (DataType::Float64, DataType::Int64)
        | (DataType::Float64, DataType::Int32)
        | (DataType::Float64, DataType::Int16)
        | (DataType::Float64, DataType::UInt64)
        | (DataType::Float64, DataType::UInt32)
        | (DataType::Float64, DataType::UInt16)
        | (DataType::Float64, DataType::UInt8) => {
            require_truncate(ctx, src, dst)?;
            float_narrow::convert(value, src, dst)
        }

        // Fixed-width int → BigInt.
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

        // Fixed-width int → Decimal.
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
        (DataType::BigInt { .. }, DataType::Decimal { .. }) => match value {
            Value::BigInt(b) => Ok(Value::Decimal(BigDecimal::new(b, 0))),
            _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
        },

        // Unsigned widening within unsigned.
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

        // Unsigned → Decimal.
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

        // ---- Integer narrowing (signed/unsigned/cross-sign) ------------
        (DataType::Int64, DataType::Int32 | DataType::Int16)
        | (DataType::Int32, DataType::Int16)
        | (DataType::UInt64, DataType::UInt32 | DataType::UInt16 | DataType::UInt8)
        | (DataType::UInt32, DataType::UInt16 | DataType::UInt8)
        | (DataType::UInt16, DataType::UInt8)
        | (
            DataType::Int16 | DataType::Int32 | DataType::Int64,
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64,
        )
        | (DataType::UInt64, DataType::Int64 | DataType::Int32 | DataType::Int16)
        | (DataType::UInt32, DataType::Int32 | DataType::Int16)
        | (DataType::UInt16, DataType::Int16) => {
            require_truncate(ctx, src, dst)?;
            int_narrow::convert(value, src, dst)
        }

        // ---- BigInt narrowing → Int*/UInt* ----------------------------
        (
            DataType::BigInt { .. },
            DataType::Int64
            | DataType::Int32
            | DataType::Int16
            | DataType::UInt64
            | DataType::UInt32
            | DataType::UInt16
            | DataType::UInt8,
        ) => {
            require_truncate(ctx, src, dst)?;
            bigint_narrow::convert(value, src, dst)
        }

        // ---- Decimal narrowing → BigInt/Int*/UInt* --------------------
        (
            DataType::Decimal { .. },
            DataType::BigInt { .. }
            | DataType::Int64
            | DataType::Int32
            | DataType::Int16
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
            xml_text::xml_to_text(value, src, *size)
        }
        (DataType::Text { .. }, DataType::Xml) => xml_text::text_to_xml(value, src),

        // ---- Timestamp → Date ---------------------------------------
        (DataType::Timestamp, DataType::Date) => {
            require_truncate(ctx, src, dst)?;
            timestamp_date::convert(value, src)
        }

        _ => Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
        }),
    }
}

/// Identity (`src == dst`) check. `Json → Json` and `Xml → Xml` with
/// `truncate=true` are forbidden — that combination would corrupt the
/// payload's structure and there is no sensible truncation to apply.
fn identity_or_forbid(
    value: Value,
    src: &DataType,
    dst: &DataType,
    ctx: &ConversionContext,
) -> Result<Value, ConvertError> {
    if ctx.truncate
        && matches!(
            src,
            DataType::Json | DataType::Xml | DataType::Uuid | DataType::Date | DataType::Timestamp
        )
        && src == dst
    {
        // Identity for these types is harmless without truncate; with
        // truncate the request is meaningless but we must error for Json
        // and Xml (corruption risk) and for Uuid/Date/Timestamp where
        // truncation has no defined semantics.
        if matches!(src, DataType::Json | DataType::Xml) {
            return Err(ConvertError::TruncationForbidden {
                src: *src,
                dst: *dst,
            });
        }
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
    let widens = match (src, dst) {
        (BigInt { width: a }, BigInt { width: b }) => widens_or_equal(*a, *b),
        (
            Decimal {
                precision: pa,
                scale: sa,
            },
            Decimal {
                precision: pb,
                scale: sb,
            },
        ) => decimal_widens_or_equal(*pa, *sa, *pb, *sb),
        _ => false,
    };
    if widens {
        return Ok(value);
    }
    if !ctx.truncate {
        return Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
        });
    }
    match (src, dst) {
        (BigInt { .. }, BigInt { .. }) => bigint_narrow::convert(value, src, dst),
        (Decimal { .. }, Decimal { .. }) => decimal_narrow::convert(value, src, dst),
        _ => Err(ConvertError::Unsupported {
            src: *src,
            dst: *dst,
        }),
    }
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
            src: *src,
            dst: *dst,
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

    #[test]
    fn identity_int32() {
        let ctx = passthrough();
        let out = convert(Value::Int32(7), &DataType::Int32, &DataType::Int32, &ctx).unwrap();
        assert_eq!(out, Value::Int32(7));
    }

    #[test]
    fn identity_int32_with_truncate_is_noop() {
        let ctx = truncate_ctx();
        let out = convert(Value::Int32(7), &DataType::Int32, &DataType::Int32, &ctx).unwrap();
        assert_eq!(out, Value::Int32(7));
    }

    #[test]
    fn text_widening_with_truncate_is_noop() {
        let out = convert(
            Value::Text("hi".into()),
            &dt_text(10),
            &dt_text(20),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("hi".into()));
    }

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
    fn text_narrow_utf8_rounds_down() {
        // "Привет" = 12 bytes, max=5 → "Пр" (4 bytes).
        let out = convert(
            Value::Text("Привет".into()),
            &dt_text(20),
            &dt_text(5),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("Пр".into()));
    }

    #[test]
    fn text_narrow_emoji_oversize_to_empty() {
        // emoji is 4 bytes; max=3 → ""
        let out = convert(
            Value::Text("😀".into()),
            &dt_text(20),
            &dt_text(3),
            &truncate_ctx(),
        )
        .unwrap();
        assert_eq!(out, Value::Text("".into()));
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
}
