//! MS SQL `IMAGE` type — a deprecated LOB binary type, functionally
//! equivalent to `VARBINARY(MAX)`. Exists as a custom type solely so
//! MS SQL → MS SQL round-trips preserve type identity without narrowing.
//!
//! IMAGE maps to `VARBINARY(MAX)` in PG/MySQL sinks (lossless widening).
//! Conversion to canonical `Bytes` is only allowed with truncation opt-in.

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

#[derive(Debug, Clone, Copy, Default)]
pub struct MssqlImageType;

impl MssqlImageType {
    pub const KIND: &'static str = "mssql.image";
}

impl DynType for MssqlImageType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        false
    }

    /// Identity accepted. Conversion to unbounded `Bytes` is allowed —
    /// IMAGE is semantically identical to `VARBINARY(MAX)`, so widening
    /// to `Bytes { size: None }` is lossless.
    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == self.kind())
            || matches!(target, DataType::Bytes { size: None })
    }

    /// Identity accepted. Construction from unbounded `Bytes` is allowed.
    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Custom(t) if t.kind() == self.kind())
            || matches!(src, DataType::Bytes { size: None })
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
            DataType::Custom(t) if t.kind() == self.kind() => Ok(value),
            DataType::Bytes { .. } => {
                if let Value::Custom(c) = value {
                    if let Some(img) = c.as_any().downcast_ref::<MssqlImageValue>() {
                        return Ok(Value::Bytes(img.0.clone()));
                    }
                }
                Err(ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(*self)),
                    dst: target.clone(),
                })
            }
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
            DataType::Custom(t) if t.kind() == self.kind() => Ok(value),
            DataType::Bytes { .. } => {
                if let Value::Bytes(b) = value {
                    return Ok(Value::Custom(Box::new(MssqlImageValue(b))));
                }
                Err(ConvertError::Unsupported {
                    src: src.clone(),
                    dst: DataType::Custom(Box::new(*self)),
                })
            }
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        Ok(None)
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone)]
pub struct MssqlImageValue(pub Vec<u8>);

impl DynValue for MssqlImageValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(MssqlImageType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        match other.as_any().downcast_ref::<MssqlImageValue>() {
            Some(o) => self.0 == o.0,
            None => false,
        }
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(BASE64_STANDARD.encode(&self.0)))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx() -> ConversionContext {
        ConversionContext::passthrough()
    }

    #[test]
    fn kind_is_stable() {
        assert_eq!(MssqlImageType.kind(), "mssql.image");
    }

    #[test]
    fn cannot_be_cursor() {
        assert!(!MssqlImageType.can_be_cursor());
    }

    #[test]
    fn identity_conversion_is_accepted() {
        let same = DataType::Custom(Box::new(MssqlImageType));
        assert!(MssqlImageType.can_convert_to(&same, false));
        assert!(MssqlImageType.can_construct_from(&same, false));
    }

    #[test]
    fn converts_to_unbounded_bytes() {
        assert!(MssqlImageType.can_convert_to(&DataType::Bytes { size: None }, false));
    }

    #[test]
    fn constructs_from_unbounded_bytes() {
        assert!(MssqlImageType.can_construct_from(&DataType::Bytes { size: None }, false));
    }

    #[test]
    fn rejects_sized_bytes_target() {
        assert!(!MssqlImageType.can_convert_to(&DataType::Bytes { size: Some(1024) }, false));
    }

    #[test]
    fn convert_identity_passes_value_through() {
        let v = Value::Custom(Box::new(MssqlImageValue(vec![1, 2, 3])));
        let same = DataType::Custom(Box::new(MssqlImageType));
        let out = MssqlImageType.convert(v.clone(), &same, &ctx()).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn convert_to_bytes_unwraps() {
        let v = Value::Custom(Box::new(MssqlImageValue(vec![4, 5, 6])));
        let out = MssqlImageType
            .convert(v, &DataType::Bytes { size: None }, &ctx())
            .unwrap();
        assert_eq!(out, Value::Bytes(vec![4, 5, 6]));
    }

    #[test]
    fn construct_from_bytes_wraps() {
        let v = Value::Bytes(vec![7, 8, 9]);
        let out = MssqlImageType
            .construct(v, &DataType::Bytes { size: None }, &ctx())
            .unwrap();
        assert_eq!(out, Value::Custom(Box::new(MssqlImageValue(vec![7, 8, 9]))));
    }

    #[test]
    fn parse_default_returns_none() {
        let lit = toml::Value::String("anything".into());
        assert!(MssqlImageType.parse_default(&lit).unwrap().is_none());
    }

    #[test]
    fn dyn_value_to_json_emits_base64() {
        let v = MssqlImageValue(vec![0xde, 0xad, 0xbe, 0xef]);
        let j = DynValue::to_json(&v).unwrap();
        assert_eq!(j, serde_json::json!("3q2+7w=="));
    }
}
