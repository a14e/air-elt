//! QuestDB `LONG256` — a 256-bit signed integer carrier rendered as a
//! `0x…`-prefixed hex string on the wire (both for pg-wire TEXT binding
//! and for JSON encoding).
//!
//! No cross-canonical conversion is offered: there is no canonical
//! 256-bit type in the pivot, and `BigInt` carries arbitrary width but
//! no fixed endianness convention. Values flow Custom→Custom only.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbLong256Type;

impl QuestDbLong256Type {
    pub const KIND: &'static str = "questdb.long256";
}

impl DynType for QuestDbLong256Type {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(*self)),
                dst: target.clone(),
            }),
        }
    }

    fn construct(
        &self,
        value: Value,
        src: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match src {
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        let raw = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(*self)),
        })?;
        // Accept either `0x` + 64 hex chars (big-endian textual convention
        // used by QuestDB itself when surfacing LONG256) or 64 bare hex
        // chars. Anything else is rejected.
        let hex_part = raw
            .strip_prefix("0x")
            .or_else(|| raw.strip_prefix("0X"))
            .unwrap_or(raw);
        if hex_part.len() != 64 {
            return Err(DefaultParseError::TypeMismatch {
                dst: DataType::Custom(Box::new(*self)),
            });
        }
        let mut be_bytes = [0u8; 32];
        for (i, chunk) in hex_part.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| DefaultParseError::TypeMismatch {
                dst: DataType::Custom(Box::new(*self)),
            })?;
            be_bytes[i] =
                u8::from_str_radix(pair, 16).map_err(|_| DefaultParseError::TypeMismatch {
                    dst: DataType::Custom(Box::new(*self)),
                })?;
        }
        // Storage layout is little-endian; reverse the parsed BE bytes.
        let mut le_bytes = [0u8; 32];
        for (i, b) in be_bytes.iter().enumerate() {
            le_bytes[31 - i] = *b;
        }
        Ok(Some(Value::Custom(Box::new(QuestDbLong256Value(le_bytes)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

/// 256-bit value stored as 32 little-endian bytes. The pg-wire binder
/// renders it as `0x<big-endian-hex>` (the textual convention used by
/// QuestDB itself when surfacing `LONG256` over pg-wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbLong256Value(pub [u8; 32]);

impl QuestDbLong256Value {
    /// Render as `0x` + 64 lowercase hex chars in big-endian byte order.
    pub fn to_hex(&self) -> String {
        let mut out = String::with_capacity(2 + 64);
        out.push_str("0x");
        // Big-endian display: high byte first. Internal storage is LE so
        // we iterate in reverse to print MSB-first.
        for byte in self.0.iter().rev() {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }
}

impl DynValue for QuestDbLong256Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(QuestDbLong256Type)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        other
            .as_any()
            .downcast_ref::<QuestDbLong256Value>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.to_hex()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn downcast_long256(value: Value) -> QuestDbLong256Value {
        match value {
            Value::Custom(b) => *b
                .into_any()
                .downcast::<QuestDbLong256Value>()
                .expect("downcast"),
            other => panic!("expected Value::Custom(QuestDbLong256Value), got {other:?}"),
        }
    }

    #[test]
    fn parse_default_accepts_0x_prefixed_64_hex() {
        let ty = QuestDbLong256Type;
        // BE textual: high byte = 0xab, low byte = 0xef.
        let mut hex = String::from("0xab");
        hex.push_str(&"00".repeat(30));
        hex.push_str("ef");
        let lit = toml::Value::String(hex.clone());
        let value = ty.parse_default(&lit).unwrap().unwrap();
        let v = downcast_long256(value);
        // Round-trip via to_hex.
        assert_eq!(v.to_hex(), hex);
    }

    #[test]
    fn parse_default_accepts_bare_64_hex() {
        let ty = QuestDbLong256Type;
        let hex = "0".repeat(64);
        let lit = toml::Value::String(hex.clone());
        let value = ty.parse_default(&lit).unwrap().unwrap();
        let v = downcast_long256(value);
        assert_eq!(v.to_hex(), format!("0x{hex}"));
    }

    #[test]
    fn parse_default_rejects_non_string() {
        let ty = QuestDbLong256Type;
        let lit = toml::Value::Integer(42);
        let err = ty.parse_default(&lit).unwrap_err();
        assert!(matches!(err, DefaultParseError::TypeMismatch { .. }));
    }

    #[test]
    fn parse_default_rejects_wrong_length() {
        let ty = QuestDbLong256Type;
        let lit = toml::Value::String("0xabcd".to_string());
        let err = ty.parse_default(&lit).unwrap_err();
        assert!(matches!(err, DefaultParseError::TypeMismatch { .. }));
    }

    #[test]
    fn parse_default_rejects_non_hex_chars() {
        let ty = QuestDbLong256Type;
        let mut s = String::from("0x");
        s.push_str(&"zz".repeat(32));
        let lit = toml::Value::String(s);
        let err = ty.parse_default(&lit).unwrap_err();
        assert!(matches!(err, DefaultParseError::TypeMismatch { .. }));
    }
}
