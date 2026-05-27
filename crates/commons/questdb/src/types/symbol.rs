//! QuestDB `SYMBOL` columns.
//!
//! QuestDB `SYMBOL` is a dictionary-encoded low-cardinality string. From
//! the canonical pivot's perspective the column carries text, but the
//! sink preserves the SYMBOL distinction so it can be carried through
//! the type model and bound as TEXT over pg-wire (QuestDB coerces
//! server-side to SYMBOL per the DDL).
//!
//! Cross-canonical conversion is allowed both ways with `Text { size: None }`
//! so users can map a generic `String` source column to a `SYMBOL` target
//! and vice versa.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbSymbolType;

impl QuestDbSymbolType {
    pub const KIND: &'static str = "questdb.symbol";
}

impl DynType for QuestDbSymbolType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Text { .. })
            || matches!(target, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Text { .. })
            || matches!(src, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
            DataType::Text { .. } => {
                let v = downcast(&value)?;
                Ok(Value::Text(v.0.clone()))
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
            DataType::Text { .. } => match value {
                Value::Text(t) => Ok(Value::Custom(Box::new(QuestDbSymbolValue(t)))),
                _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
            },
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, String> {
        let s = literal
            .as_str()
            .ok_or_else(|| "expected string literal for symbol default".to_owned())?;
        Ok(Some(Value::Custom(Box::new(QuestDbSymbolValue(
            s.to_string(),
        )))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestDbSymbolValue(pub String);

impl DynValue for QuestDbSymbolValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(QuestDbSymbolType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }

    fn is_equal(&self, other: &dyn DynValue) -> bool {
        other
            .as_any()
            .downcast_ref::<QuestDbSymbolValue>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.clone()))
    }
}

fn downcast(value: &Value) -> Result<&QuestDbSymbolValue, ConvertError> {
    match value {
        Value::Custom(b) => b
            .as_any()
            .downcast_ref::<QuestDbSymbolValue>()
            .ok_or_else(|| ConvertError::ValueShapeMismatch {
                src: DataType::Custom(Box::new(QuestDbSymbolType)),
            }),
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(QuestDbSymbolType)),
        }),
    }
}
