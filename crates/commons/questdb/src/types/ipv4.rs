//! QuestDB `IPv4` columns. Stored on the wire as a dotted-quad textual
//! value via pg-wire. Cross-canonical conversion to/from canonical
//! [`DataType::Text`] is offered for ergonomics (so users can map a
//! `VARCHAR` source column to an `IPv4` sink column and vice versa).

use std::any::Any;
use std::net::Ipv4Addr;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbIpv4Type;

impl QuestDbIpv4Type {
    pub const KIND: &'static str = "questdb.ipv4";
}

impl DynType for QuestDbIpv4Type {
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
                Ok(Value::Text(v.0.to_string()))
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
            DataType::Text { .. } => {
                let s = match value {
                    Value::Text(t) => t,
                    _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
                };
                let addr: Ipv4Addr = s.parse().map_err(|_| ConvertError::Unsupported {
                    src: src.clone(),
                    dst: DataType::Custom(Box::new(*self)),
                })?;
                Ok(Value::Custom(Box::new(QuestDbIpv4Value(addr))))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(*self)),
        })?;
        let addr: Ipv4Addr = s.parse().map_err(|_| DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(*self)),
        })?;
        Ok(Some(Value::Custom(Box::new(QuestDbIpv4Value(addr)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbIpv4Value(pub Ipv4Addr);

impl DynValue for QuestDbIpv4Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(QuestDbIpv4Type)
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
            .downcast_ref::<QuestDbIpv4Value>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.to_string()))
    }
}

fn downcast(value: &Value) -> Result<&QuestDbIpv4Value, ConvertError> {
    match value {
        Value::Custom(b) => b
            .as_any()
            .downcast_ref::<QuestDbIpv4Value>()
            .ok_or_else(|| ConvertError::ValueShapeMismatch {
                src: DataType::Custom(Box::new(QuestDbIpv4Type)),
            }),
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(QuestDbIpv4Type)),
        }),
    }
}
