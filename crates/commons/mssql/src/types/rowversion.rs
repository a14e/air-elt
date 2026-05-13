//! MS SQL `ROWVERSION` / `TIMESTAMP` type — an auto-generated 8-byte binary
//! value that changes on every row update. Used for optimistic concurrency.
//!
//! ROWVERSION is read-only: the server generates it, users cannot insert or
//! update it. Air Elt excludes ROWVERSION columns from INSERT column lists.
//! It is identity-only — bytes copy byte-for-byte between MS SQL endpoints;
//! no conversion to/from canonical types is supported.

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
pub struct MssqlRowVersionType;

impl MssqlRowVersionType {
    pub const KIND: &'static str = "mssql.rowversion";
}

impl DynType for MssqlRowVersionType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        false
    }

    fn fixed_size(&self) -> Option<u32> {
        Some(8)
    }

    /// Identity only: `rowversion → rowversion`.
    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == self.kind())
    }

    /// Identity only: `rowversion ← rowversion`.
    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Custom(t) if t.kind() == self.kind())
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        if matches!(target, DataType::Custom(t) if t.kind() == self.kind()) {
            return Ok(value);
        }
        Err(ConvertError::Unsupported {
            src: DataType::Custom(Box::new(*self)),
            dst: target.clone(),
        })
    }

    fn construct(
        &self,
        value: Value,
        src: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        if matches!(src, DataType::Custom(t) if t.kind() == self.kind()) {
            return Ok(value);
        }
        Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: DataType::Custom(Box::new(*self)),
        })
    }

    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        Ok(None)
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone)]
pub struct MssqlRowVersionValue(pub Vec<u8>);

impl DynValue for MssqlRowVersionValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(MssqlRowVersionType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        match other.as_any().downcast_ref::<MssqlRowVersionValue>() {
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
        assert_eq!(MssqlRowVersionType.kind(), "mssql.rowversion");
    }

    #[test]
    fn cannot_be_cursor() {
        assert!(!MssqlRowVersionType.can_be_cursor());
    }

    #[test]
    fn fixed_size_is_8() {
        assert_eq!(MssqlRowVersionType.fixed_size(), Some(8));
    }

    #[test]
    fn identity_conversion_is_accepted() {
        let same = DataType::Custom(Box::new(MssqlRowVersionType));
        assert!(MssqlRowVersionType.can_convert_to(&same, false));
        assert!(MssqlRowVersionType.can_construct_from(&same, false));
    }

    #[test]
    fn rejects_non_identity_targets() {
        let rv = MssqlRowVersionType;
        for target in [
            DataType::Bytes { size: None },
            DataType::Text { size: None },
        ] {
            assert!(!rv.can_convert_to(&target, false));
            assert!(!rv.can_construct_from(&target, false));
        }
    }

    #[test]
    fn convert_identity_passes_value_through() {
        let v = Value::Custom(Box::new(MssqlRowVersionValue(vec![1, 2, 3, 4, 5, 6, 7, 8])));
        let same = DataType::Custom(Box::new(MssqlRowVersionType));
        let out = MssqlRowVersionType
            .convert(v.clone(), &same, &ctx())
            .unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn parse_default_returns_none() {
        let lit = toml::Value::String("anything".into());
        assert!(MssqlRowVersionType.parse_default(&lit).unwrap().is_none());
    }

    #[test]
    fn dyn_value_to_json_emits_base64() {
        let v = MssqlRowVersionValue(vec![0xde, 0xad, 0xbe, 0xef, 0, 0, 0, 0]);
        let j = DynValue::to_json(&v).unwrap();
        assert_eq!(j, serde_json::json!("3q2+7wAAAAA="));
    }
}
