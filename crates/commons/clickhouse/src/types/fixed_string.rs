//! `FixedString(N)` — exactly-N-byte string. ClickHouse pads / rejects
//! values that don't fit the declared length.
//!
//! Cross-canonical conversion: `FixedString(N) ↔ Bytes(N)`. Bytes round
//! trip lossless. `Text` conversions are *not* offered: many FixedString
//! columns store non-UTF8 binary data (hashes, fingerprints), and a
//! silent UTF-8 reinterpretation would corrupt them.

use std::any::Any;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChFixedStringType {
    pub size: u32,
}

impl ChFixedStringType {
    pub const KIND: &'static str = "clickhouse.fixed_string";
}

impl DynType for ChFixedStringType {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn display(&self) -> String {
        format!("FixedString({})", self.size)
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        match target {
            DataType::Bytes { size } => size.map(|n| n == self.size).unwrap_or(true),
            DataType::Custom(t) if t.kind() == Self::KIND => true,
            _ => false,
        }
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        match src {
            DataType::Bytes { size } => size.map(|n| n == self.size).unwrap_or(true),
            DataType::Custom(t) if t.kind() == Self::KIND => true,
            _ => false,
        }
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
            DataType::Bytes { .. } => {
                let v = match value {
                    Value::Custom(b) => b,
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch {
                            src: DataType::Custom(Box::new(*self)),
                        });
                    }
                };
                let cast = v.into_any().downcast::<ChFixedStringValue>().map_err(|_| {
                    ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(*self)),
                        dst: target.clone(),
                    }
                })?;
                Ok(Value::Bytes(cast.bytes))
            }
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
            DataType::Bytes { .. } => {
                let b = match value {
                    Value::Bytes(b) => b,
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
                    }
                };
                if b.len() != self.size as usize {
                    return Err(ConvertError::Length {
                        expected: self.size as usize,
                        got: b.len(),
                    });
                }
                Ok(Value::Custom(Box::new(ChFixedStringValue { bytes: b })))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        // No TOML literal grammar for opaque FixedString — operator
        // should map a Bytes column with a `hex:` / `base64:` default
        // through the canonical pivot if a default is needed.
        Ok(None)
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChFixedStringValue {
    pub bytes: Vec<u8>,
}

impl DynValue for ChFixedStringValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChFixedStringType {
            size: self.bytes.len() as u32,
        })
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
            .downcast_ref::<ChFixedStringValue>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(
            BASE64_STANDARD.encode(&self.bytes),
        ))
    }
}
