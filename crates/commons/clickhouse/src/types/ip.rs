//! `IPv4` and `IPv6` columns.
//!
//! Cross-canonical conversions are allowed both ways:
//! * IPv4 ↔ `Text` (canonical "x.x.x.x").
//! * IPv6 ↔ `Text` (canonical RFC 5952 lowercase).
//!
//! Bytes-form conversion is intentionally skipped for v1 — the
//! canonical `Bytes(4)` / `Bytes(16)` round-trip would shadow the more
//! useful textual encoding without operator benefit.

use std::any::Any;
use std::net::{Ipv4Addr, Ipv6Addr};

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

// ---------------------------------------------------------------- IPv4

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChIpv4Type;

impl ChIpv4Type {
    pub const KIND: &'static str = "clickhouse.ipv4";
}

impl DynType for ChIpv4Type {
    fn kind(&self) -> &'static str {
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
                let v = downcast_ipv4(&value, target)?;
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
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
                    }
                };
                let parsed: Ipv4Addr = s.parse().map_err(|_| ConvertError::Unsupported {
                    src: src.clone(),
                    dst: DataType::Custom(Box::new(*self)),
                })?;
                Ok(Value::Custom(Box::new(ChIpv4Value(parsed))))
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
        let v: Ipv4Addr = s.parse().map_err(|_| DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(*self)),
        })?;
        Ok(Some(Value::Custom(Box::new(ChIpv4Value(v)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChIpv4Value(pub Ipv4Addr);

impl DynValue for ChIpv4Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChIpv4Type)
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
            .downcast_ref::<ChIpv4Value>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.to_string()))
    }
}

fn downcast_ipv4<'a>(value: &'a Value, target: &DataType) -> Result<&'a ChIpv4Value, ConvertError> {
    match value {
        Value::Custom(b) => {
            b.as_any()
                .downcast_ref::<ChIpv4Value>()
                .ok_or_else(|| ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(ChIpv4Type)),
                    dst: target.clone(),
                })
        }
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ChIpv4Type)),
        }),
    }
}

// ---------------------------------------------------------------- IPv6

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChIpv6Type;

impl ChIpv6Type {
    pub const KIND: &'static str = "clickhouse.ipv6";
}

impl DynType for ChIpv6Type {
    fn kind(&self) -> &'static str {
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
                let v = downcast_ipv6(&value, target)?;
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
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
                    }
                };
                let parsed: Ipv6Addr = s.parse().map_err(|_| ConvertError::Unsupported {
                    src: src.clone(),
                    dst: DataType::Custom(Box::new(*self)),
                })?;
                Ok(Value::Custom(Box::new(ChIpv6Value(parsed))))
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
        let v: Ipv6Addr = s.parse().map_err(|_| DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(*self)),
        })?;
        Ok(Some(Value::Custom(Box::new(ChIpv6Value(v)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChIpv6Value(pub Ipv6Addr);

impl DynValue for ChIpv6Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChIpv6Type)
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
            .downcast_ref::<ChIpv6Value>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.to_string()))
    }
}

fn downcast_ipv6<'a>(value: &'a Value, target: &DataType) -> Result<&'a ChIpv6Value, ConvertError> {
    match value {
        Value::Custom(b) => {
            b.as_any()
                .downcast_ref::<ChIpv6Value>()
                .ok_or_else(|| ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(ChIpv6Type)),
                    dst: target.clone(),
                })
        }
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ChIpv6Type)),
        }),
    }
}
