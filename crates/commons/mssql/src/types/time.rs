//! MS SQL `TIME` (without timezone) — modelled as a custom type because the
//! canonical type pivot has no time-of-day variant. Identity-only round-trip
//! between MS SQL endpoints; conversion to/from canonical types is not
//! supported.

use std::any::Any;

use chrono::NaiveTime;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, Copy, Default)]
pub struct MssqlTimeType;

impl MssqlTimeType {
    pub const KIND: &'static str = "mssql.time";
}

impl DynType for MssqlTimeType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        false
    }

    /// Identity only: `time → time`.
    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == self.kind())
    }

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
pub struct MssqlTimeValue(pub NaiveTime);

impl DynValue for MssqlTimeValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(MssqlTimeType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        match other.as_any().downcast_ref::<MssqlTimeValue>() {
            Some(o) => self.0 == o.0,
            None => false,
        }
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        // ISO 8601 time format: "HH:MM:SS.fffffffff"
        Ok(serde_json::Value::String(
            self.0.format("%H:%M:%S%.f").to_string(),
        ))
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
        assert_eq!(MssqlTimeType.kind(), "mssql.time");
    }

    #[test]
    fn cannot_be_cursor() {
        assert!(!MssqlTimeType.can_be_cursor());
    }

    #[test]
    fn identity_conversion_is_accepted() {
        let same = DataType::Custom(Box::new(MssqlTimeType));
        assert!(MssqlTimeType.can_convert_to(&same, false));
        assert!(MssqlTimeType.can_construct_from(&same, false));
    }

    #[test]
    fn rejects_non_identity_targets() {
        let t = MssqlTimeType;
        for target in [
            DataType::Text { size: None },
            DataType::Timestamp,
            DataType::Date,
        ] {
            assert!(!t.can_convert_to(&target, false));
            assert!(!t.can_construct_from(&target, false));
        }
    }

    #[test]
    fn convert_identity_passes_value_through() {
        let nt = NaiveTime::from_hms_opt(12, 34, 56).unwrap();
        let v = Value::Custom(Box::new(MssqlTimeValue(nt)));
        let same = DataType::Custom(Box::new(MssqlTimeType));
        let out = MssqlTimeType.convert(v.clone(), &same, &ctx()).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn parse_default_returns_none() {
        let lit = toml::Value::String("anything".into());
        assert!(MssqlTimeType.parse_default(&lit).unwrap().is_none());
    }

    #[test]
    fn dyn_value_to_json_emits_iso8601() {
        let nt = NaiveTime::from_hms_opt(12, 34, 56).unwrap();
        let v = MssqlTimeValue(nt);
        let j = DynValue::to_json(&v).unwrap();
        assert_eq!(j, serde_json::json!("12:34:56"));
    }
}
