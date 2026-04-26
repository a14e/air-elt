//! Parse a `default` literal from TOML against the resolved sink
//! `DataType` and return a typed [`Value`].
//!
//! Bytes columns use a typed prefix grammar (`hex:` / `base64:` / `utf8:`
//! / `bin:`); other types accept the plain TOML literal. Length / range /
//! shape mismatches raise [`DefaultParseError`] which the validation
//! pipeline wraps into `ValidationError::DefaultParse`.

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use std::str::FromStr;
use uuid::Uuid;

use crate::types::{DataType, Value};

#[derive(Debug, thiserror::Error)]
pub enum DefaultParseError {
    #[error("Bytes default requires one of the prefixes hex:/base64:/utf8:/bin:")]
    MissingPrefix,
    #[error("unknown Bytes default prefix; expected hex:/base64:/utf8:/bin:")]
    UnknownPrefix,
    #[error("invalid hex: {reason}")]
    InvalidHex { reason: String },
    #[error("invalid base64: {reason}")]
    InvalidBase64 { reason: String },
    #[error("invalid binary literal: {reason}")]
    InvalidBinary { reason: String },
    #[error("default exceeds the column's declared length: {got} > {max}")]
    LengthExceeds { got: usize, max: usize },
    #[error("default value out of range for {dst}")]
    OutOfRange { dst: DataType },
    #[error("default value is negative for unsigned column {dst}")]
    SignLoss { dst: DataType },
    #[error("default scale {scale} exceeds column scale {max}")]
    ScaleExceeds { scale: u32, max: u32 },
    #[error("invalid date literal: {reason}")]
    InvalidDate { reason: String },
    #[error("invalid timestamp literal: {reason}")]
    InvalidTimestamp { reason: String },
    #[error("invalid uuid literal: {reason}")]
    InvalidUuid { reason: String },
    #[error("invalid xml literal: {reason}")]
    InvalidXml { reason: String },
    #[error("default literal type does not match sink {dst}")]
    TypeMismatch { dst: DataType },
}

pub fn parse(literal: &toml::Value, sink: &DataType) -> Result<Value, DefaultParseError> {
    match sink {
        DataType::Bytes { size } => parse_bytes(literal, *size),
        DataType::Text { size } => parse_text(literal, *size),
        DataType::Bool => parse_bool(literal),
        DataType::Int16 => parse_signed(literal, sink, i16::MIN as i64, i16::MAX as i64)
            .map(|n| Value::Int16(n as i16)),
        DataType::Int32 => parse_signed(literal, sink, i32::MIN as i64, i32::MAX as i64)
            .map(|n| Value::Int32(n as i32)),
        DataType::Int64 => parse_signed(literal, sink, i64::MIN, i64::MAX).map(Value::Int64),
        DataType::UInt8 => {
            parse_unsigned(literal, sink, u8::MAX as u64).map(|n| Value::UInt8(n as u8))
        }
        DataType::UInt16 => {
            parse_unsigned(literal, sink, u16::MAX as u64).map(|n| Value::UInt16(n as u16))
        }
        DataType::UInt32 => {
            parse_unsigned(literal, sink, u32::MAX as u64).map(|n| Value::UInt32(n as u32))
        }
        DataType::UInt64 => parse_unsigned(literal, sink, u64::MAX).map(Value::UInt64),
        DataType::Float32 => parse_float(literal, sink).map(|f| Value::Float32(f as f32)),
        DataType::Float64 => parse_float(literal, sink).map(Value::Float64),
        DataType::BigInt { width } => parse_bigint(literal, sink, *width),
        DataType::Decimal { precision, scale } => parse_decimal(literal, sink, *precision, *scale),
        DataType::Date => parse_date(literal),
        DataType::Timestamp => parse_timestamp(literal),
        DataType::Uuid => parse_uuid(literal),
        DataType::Json => Ok(Value::Json(toml_to_json(literal))),
        DataType::Xml => parse_xml(literal),
    }
}

fn parse_bytes(literal: &toml::Value, size: Option<u32>) -> Result<Value, DefaultParseError> {
    let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
        dst: DataType::Bytes { size },
    })?;
    let (prefix, rest) = s.split_once(':').ok_or(DefaultParseError::MissingPrefix)?;
    let bytes = match prefix {
        "hex" => decode_hex(rest)?,
        "base64" => {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(rest)
                .map_err(|e| DefaultParseError::InvalidBase64 {
                    reason: e.to_string(),
                })?
        }
        "utf8" => rest.as_bytes().to_vec(),
        "bin" => decode_bin(rest)?,
        _ => return Err(DefaultParseError::UnknownPrefix),
    };
    if let Some(max) = size
        && bytes.len() > max as usize
    {
        return Err(DefaultParseError::LengthExceeds {
            got: bytes.len(),
            max: max as usize,
        });
    }
    Ok(Value::Bytes(bytes))
}

fn decode_hex(s: &str) -> Result<Vec<u8>, DefaultParseError> {
    if !s.len().is_multiple_of(2) {
        return Err(DefaultParseError::InvalidHex {
            reason: "odd number of hex digits".into(),
        });
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in s.as_bytes().chunks_exact(2) {
        let hi = hex_digit(chunk[0])?;
        let lo = hex_digit(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_digit(b: u8) -> Result<u8, DefaultParseError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(10 + b - b'a'),
        b'A'..=b'F' => Ok(10 + b - b'A'),
        _ => Err(DefaultParseError::InvalidHex {
            reason: format!("non-hex byte 0x{b:02x}"),
        }),
    }
}

fn decode_bin(s: &str) -> Result<Vec<u8>, DefaultParseError> {
    let bytes = s.as_bytes();
    if !bytes.iter().all(|b| *b == b'0' || *b == b'1') {
        return Err(DefaultParseError::InvalidBinary {
            reason: "expected only 0/1 with no whitespace".into(),
        });
    }
    if !bytes.len().is_multiple_of(8) {
        return Err(DefaultParseError::InvalidBinary {
            reason: "length must be a multiple of 8 bits".into(),
        });
    }
    let mut out = Vec::with_capacity(bytes.len() / 8);
    for chunk in bytes.chunks_exact(8) {
        let mut byte = 0u8;
        for &b in chunk {
            byte = (byte << 1) | (b - b'0');
        }
        out.push(byte);
    }
    Ok(out)
}

fn parse_text(literal: &toml::Value, size: Option<u32>) -> Result<Value, DefaultParseError> {
    let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
        dst: DataType::Text { size },
    })?;
    if let Some(max) = size {
        let chars = s.chars().count();
        if chars > max as usize {
            return Err(DefaultParseError::LengthExceeds {
                got: chars,
                max: max as usize,
            });
        }
    }
    Ok(Value::Text(s.to_string()))
}

fn parse_bool(literal: &toml::Value) -> Result<Value, DefaultParseError> {
    literal
        .as_bool()
        .map(Value::Bool)
        .ok_or(DefaultParseError::TypeMismatch {
            dst: DataType::Bool,
        })
}

fn parse_signed(
    literal: &toml::Value,
    sink: &DataType,
    min: i64,
    max: i64,
) -> Result<i64, DefaultParseError> {
    let n = literal
        .as_integer()
        .ok_or(DefaultParseError::TypeMismatch { dst: *sink })?;
    if n < min || n > max {
        return Err(DefaultParseError::OutOfRange { dst: *sink });
    }
    Ok(n)
}

fn parse_unsigned(
    literal: &toml::Value,
    sink: &DataType,
    max: u64,
) -> Result<u64, DefaultParseError> {
    let n = literal
        .as_integer()
        .ok_or(DefaultParseError::TypeMismatch { dst: *sink })?;
    if n < 0 {
        return Err(DefaultParseError::SignLoss { dst: *sink });
    }
    if (n as u64) > max {
        return Err(DefaultParseError::OutOfRange { dst: *sink });
    }
    Ok(n as u64)
}

fn parse_float(literal: &toml::Value, sink: &DataType) -> Result<f64, DefaultParseError> {
    if let Some(f) = literal.as_float() {
        return Ok(f);
    }
    if let Some(i) = literal.as_integer() {
        return Ok(i as f64);
    }
    Err(DefaultParseError::TypeMismatch { dst: *sink })
}

fn parse_bigint(
    literal: &toml::Value,
    sink: &DataType,
    width: Option<u32>,
) -> Result<Value, DefaultParseError> {
    let b = if let Some(s) = literal.as_str() {
        BigInt::from_str(s).map_err(|_| DefaultParseError::OutOfRange { dst: *sink })?
    } else if let Some(n) = literal.as_integer() {
        BigInt::from(n)
    } else {
        return Err(DefaultParseError::TypeMismatch { dst: *sink });
    };
    if let Some(w) = width {
        let max = bigint_pow10(w) - BigInt::from(1);
        if b > max || b < -max.clone() {
            return Err(DefaultParseError::OutOfRange { dst: *sink });
        }
    }
    Ok(Value::BigInt(b))
}

fn parse_decimal(
    literal: &toml::Value,
    sink: &DataType,
    precision: Option<u32>,
    scale: Option<u32>,
) -> Result<Value, DefaultParseError> {
    let d = if let Some(s) = literal.as_str() {
        BigDecimal::from_str(s).map_err(|_| DefaultParseError::TypeMismatch { dst: *sink })?
    } else if let Some(n) = literal.as_integer() {
        BigDecimal::from(n)
    } else if let Some(f) = literal.as_float() {
        BigDecimal::from_str(&f.to_string())
            .map_err(|_| DefaultParseError::TypeMismatch { dst: *sink })?
    } else {
        return Err(DefaultParseError::TypeMismatch { dst: *sink });
    };
    // Scale check uses the *significant* fractional digit count: trailing
    // zeros (e.g. "12.30" against decimal(p, 1)) are not a real scale
    // overflow, so we normalise first to strip them. `BigDecimal::normalized`
    // collapses `12.30` to mantissa-3 scale-1 (or `12.300` to scale-1),
    // matching how databases evaluate the literal.
    if let Some(max_scale) = scale {
        let normalized = d.normalized();
        let actual_scale: i64 = normalized.fractional_digit_count();
        if actual_scale > 0 && (actual_scale as u32) > max_scale {
            return Err(DefaultParseError::ScaleExceeds {
                scale: actual_scale as u32,
                max: max_scale,
            });
        }
    }
    // precision check (integer digits)
    if let Some(p) = precision {
        let s = scale.unwrap_or(0);
        let int_digits = p.saturating_sub(s);
        let abs = d.abs();
        let one = BigDecimal::from(1);
        if abs >= one * bigdecimal_pow10(int_digits) {
            return Err(DefaultParseError::OutOfRange { dst: *sink });
        }
    }
    Ok(Value::Decimal(d))
}

fn parse_date(literal: &toml::Value) -> Result<Value, DefaultParseError> {
    let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
        dst: DataType::Date,
    })?;
    NaiveDate::from_str(s)
        .map(Value::Date)
        .map_err(|e| DefaultParseError::InvalidDate {
            reason: e.to_string(),
        })
}

fn parse_timestamp(literal: &toml::Value) -> Result<Value, DefaultParseError> {
    let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
        dst: DataType::Timestamp,
    })?;
    DateTime::parse_from_rfc3339(s)
        .map(|dt| Value::Timestamp(dt.with_timezone(&Utc)))
        .map_err(|e| DefaultParseError::InvalidTimestamp {
            reason: e.to_string(),
        })
}

fn parse_uuid(literal: &toml::Value) -> Result<Value, DefaultParseError> {
    let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
        dst: DataType::Uuid,
    })?;
    Uuid::parse_str(s)
        .map(Value::Uuid)
        .map_err(|e| DefaultParseError::InvalidUuid {
            reason: e.to_string(),
        })
}

fn parse_xml(literal: &toml::Value) -> Result<Value, DefaultParseError> {
    let s = literal
        .as_str()
        .ok_or(DefaultParseError::TypeMismatch { dst: DataType::Xml })?;
    crate::types::convert::xml_validate::validate(s)
        .map_err(|reason| DefaultParseError::InvalidXml { reason })?;
    Ok(Value::Text(s.to_string()))
}

fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    use toml::Value::*;
    match v {
        String(s) => serde_json::Value::String(s.clone()),
        Integer(n) => serde_json::Value::Number((*n).into()),
        Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Boolean(b) => serde_json::Value::Bool(*b),
        Datetime(d) => serde_json::Value::String(d.to_string()),
        Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

fn bigint_pow10(exp: u32) -> BigInt {
    let mut out = BigInt::from(1);
    let ten = BigInt::from(10);
    for _ in 0..exp {
        out *= &ten;
    }
    out
}

fn bigdecimal_pow10(exp: u32) -> BigDecimal {
    BigDecimal::new(BigInt::from(1), -(exp as i64))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn lit(s: &str) -> toml::Value {
        toml::Value::String(s.into())
    }

    #[test]
    fn bytes_hex_ok() {
        let v = parse(
            &lit("hex:0102030405060708"),
            &DataType::Bytes { size: Some(8) },
        )
        .unwrap();
        assert_eq!(v, Value::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8]));
    }

    #[test]
    fn bytes_hex_odd_length_rejected() {
        let res = parse(&lit("hex:01020"), &DataType::Bytes { size: Some(8) });
        assert!(matches!(res, Err(DefaultParseError::InvalidHex { .. })));
    }

    #[test]
    fn bytes_hex_too_long_rejected() {
        let res = parse(&lit("hex:01020304"), &DataType::Bytes { size: Some(2) });
        assert!(matches!(res, Err(DefaultParseError::LengthExceeds { .. })));
    }

    #[test]
    fn bytes_base64_ok() {
        let v = parse(
            &lit("base64:AQIDBAUGBwg="),
            &DataType::Bytes { size: Some(8) },
        )
        .unwrap();
        assert_eq!(v, Value::Bytes(vec![1, 2, 3, 4, 5, 6, 7, 8]));
    }

    #[test]
    fn bytes_utf8_ok() {
        let v = parse(&lit("utf8:hello"), &DataType::Bytes { size: Some(10) }).unwrap();
        assert_eq!(v, Value::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn bytes_utf8_too_long() {
        let res = parse(&lit("utf8:Привет"), &DataType::Bytes { size: Some(10) });
        assert!(matches!(res, Err(DefaultParseError::LengthExceeds { .. })));
    }

    #[test]
    fn bytes_bin_ok() {
        let v = parse(
            &lit("bin:01010101111100000000111110101010"),
            &DataType::Bytes { size: Some(4) },
        )
        .unwrap();
        assert_eq!(v, Value::Bytes(vec![0x55, 0xF0, 0x0F, 0xAA]));
    }

    #[test]
    fn bytes_bin_whitespace_rejected() {
        let res = parse(&lit("bin:0101 0101"), &DataType::Bytes { size: Some(2) });
        assert!(matches!(res, Err(DefaultParseError::InvalidBinary { .. })));
    }

    #[test]
    fn bytes_bin_misaligned_rejected() {
        let res = parse(&lit("bin:0101"), &DataType::Bytes { size: Some(1) });
        assert!(matches!(res, Err(DefaultParseError::InvalidBinary { .. })));
    }

    #[test]
    fn bytes_no_prefix_rejected() {
        let res = parse(&lit("hello"), &DataType::Bytes { size: Some(8) });
        assert!(matches!(res, Err(DefaultParseError::MissingPrefix)));
    }

    #[test]
    fn bytes_unknown_prefix_rejected() {
        let res = parse(&lit("oct:777"), &DataType::Bytes { size: Some(8) });
        assert!(matches!(res, Err(DefaultParseError::UnknownPrefix)));
    }

    #[test]
    fn text_no_prefix_grammar() {
        let v = parse(&lit("hex:01"), &DataType::Text { size: Some(10) }).unwrap();
        assert_eq!(v, Value::Text("hex:01".into()));
    }

    #[test]
    fn text_overflow_rejected() {
        let res = parse(&lit("hello"), &DataType::Text { size: Some(3) });
        assert!(matches!(res, Err(DefaultParseError::LengthExceeds { .. })));
    }

    #[test]
    fn int_out_of_range_rejected() {
        let res = parse(&toml::Value::Integer(40_000), &DataType::Int16);
        assert!(matches!(res, Err(DefaultParseError::OutOfRange { .. })));
    }

    #[test]
    fn unsigned_negative_rejected() {
        let res = parse(&toml::Value::Integer(-1), &DataType::UInt8);
        assert!(matches!(res, Err(DefaultParseError::SignLoss { .. })));
    }

    #[test]
    fn bool_strict() {
        assert_eq!(
            parse(&toml::Value::Boolean(true), &DataType::Bool).unwrap(),
            Value::Bool(true)
        );
        let res = parse(&toml::Value::Integer(1), &DataType::Bool);
        assert!(matches!(res, Err(DefaultParseError::TypeMismatch { .. })));
    }

    #[test]
    fn date_ok_invalid_rejected() {
        let v = parse(&lit("2024-01-15"), &DataType::Date).unwrap();
        assert!(matches!(v, Value::Date(_)));
        let res = parse(&lit("2024-02-30"), &DataType::Date);
        assert!(matches!(res, Err(DefaultParseError::InvalidDate { .. })));
    }

    #[test]
    fn uuid_ok_invalid_rejected() {
        let v = parse(
            &lit("550e8400-e29b-41d4-a716-446655440000"),
            &DataType::Uuid,
        )
        .unwrap();
        assert!(matches!(v, Value::Uuid(_)));
        let res = parse(&lit("garbage"), &DataType::Uuid);
        assert!(matches!(res, Err(DefaultParseError::InvalidUuid { .. })));
    }

    #[test]
    fn xml_ok_invalid_rejected() {
        let v = parse(&lit("<root/>"), &DataType::Xml).unwrap();
        assert!(matches!(v, Value::Text(_)));
        let res = parse(&lit("<root>"), &DataType::Xml);
        assert!(matches!(res, Err(DefaultParseError::InvalidXml { .. })));
    }

    #[test]
    fn decimal_scale_exceeds() {
        let res = parse(
            &lit("12.345"),
            &DataType::Decimal {
                precision: Some(10),
                scale: Some(2),
            },
        );
        assert!(matches!(res, Err(DefaultParseError::ScaleExceeds { .. })));
    }

    /// Regression: trailing-zero fractional digits should NOT count toward
    /// the scale check. `"12.30"` has significant scale 1, so it fits a
    /// decimal(p, 1) sink.
    #[test]
    fn decimal_trailing_zero_normalised_for_scale_check() {
        let v = parse(
            &lit("12.30"),
            &DataType::Decimal {
                precision: Some(10),
                scale: Some(1),
            },
        )
        .unwrap();
        assert!(matches!(v, Value::Decimal(_)));
    }

    /// Regression: XML with multiple top-level elements is not well-formed
    /// per XML 1.0 ("document" production).
    #[test]
    fn xml_multiple_roots_rejected() {
        let res = parse(&lit("<a/><b/>"), &DataType::Xml);
        assert!(matches!(res, Err(DefaultParseError::InvalidXml { .. })));
    }
}
