//! `mongodb.javascript` custom type.
//!
//! BSON `JavaScriptCode` carries a UTF-8 string with no extra metadata
//! (the scoped variant is intentionally not supported here — Mongo
//! deprecated it server-side, and our pipeline has no use for the
//! attached scope document). The custom type exists so that the source
//! and sink preserve the BSON variant: a `Bson::JavaScriptCode("…")`
//! read by the source must be written back as `Bson::JavaScriptCode`,
//! not as a plain string, when the sink schema also declares the
//! column as `mongodb.javascript`.
//!
//! ## Conversion matrix
//!
//! Bidirectional with `Text { * }` and `Bytes { * }` (UTF-8 codec) —
//! lossless in both directions for any size annotation. Runtime
//! decode of `Bytes` validates UTF-8; non-UTF-8 input errors.
//!
//! ## Cursor
//!
//! `can_be_cursor() = false`. JavaScript code is a free-form string
//! with no useful order semantics for ELT purposes.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

/// Schema-side descriptor for `mongodb.javascript`.
#[derive(Debug, Clone, Copy)]
pub struct MongoJsType;

/// Runtime carrier for a JavaScript code string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MongoJsValue(pub String);

impl MongoJsType {
    /// Single source of truth for the kind string.
    pub const KIND: &'static str = "mongodb.javascript";
}

impl DynType for MongoJsType {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        false
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        // Outbound direction: code body is arbitrary length, sink
        // must be unbounded. A bounded sink would silently overflow
        // (or backend-truncate) for any code body longer than the
        // declared width, so we reject at validation time.
        matches!(
            target,
            DataType::Text { size: None } | DataType::Bytes { size: None }
        )
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        // Inbound: any string/bytes column can produce JS — we just
        // wrap the value verbatim (with utf8 validation for bytes).
        matches!(src, DataType::Text { .. } | DataType::Bytes { .. })
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        let s = unwrap_js(&value)?.0.clone();
        match target {
            DataType::Text { .. } => Ok(Value::Text(s)),
            DataType::Bytes { .. } => Ok(Value::Bytes(s.into_bytes())),
            other => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(MongoJsType)),
                dst: other.clone(),
            }),
        }
    }

    fn construct(
        &self,
        value: Value,
        src: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match (value, src) {
            (Value::Text(s), DataType::Text { .. }) => Ok(Value::Custom(Box::new(MongoJsValue(s)))),
            (Value::Bytes(b), DataType::Bytes { .. }) => {
                let s = String::from_utf8(b).map_err(|_| ConvertError::Unsupported {
                    src: DataType::Bytes { size: None },
                    dst: DataType::Custom(Box::new(MongoJsType)),
                })?;
                Ok(Value::Custom(Box::new(MongoJsValue(s))))
            }
            (_, other) => Err(ConvertError::Unsupported {
                src: other.clone(),
                dst: DataType::Custom(Box::new(MongoJsType)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(MongoJsType)),
        })?;
        Ok(Some(Value::Custom(Box::new(MongoJsValue(s.to_string())))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

impl DynValue for MongoJsValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(MongoJsType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        other
            .as_any()
            .downcast_ref::<MongoJsValue>()
            .map(|o| o.0 == self.0)
            .unwrap_or(false)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    /// JSON auto-pack encoding: emit the code body verbatim as a JSON
    /// string. Mongo wire format would distinguish JS code from a
    /// plain string; the JSON-pack pipeline does not.
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.clone()))
    }
}

fn unwrap_js(v: &Value) -> Result<&MongoJsValue, ConvertError> {
    match v {
        Value::Custom(inner) => inner
            .as_any()
            .downcast_ref::<MongoJsValue>()
            .ok_or_else(|| ConvertError::ValueShapeMismatch {
                src: DataType::Custom(Box::new(MongoJsType)),
            }),
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(MongoJsType)),
        }),
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
        assert_eq!(MongoJsType.kind(), "mongodb.javascript");
    }

    #[test]
    fn cannot_be_cursor() {
        assert!(!MongoJsType.can_be_cursor());
    }

    #[test]
    fn convert_to_text_returns_owned_string() {
        let code = "function () { return 1; }";
        let out = MongoJsType
            .convert(
                Value::Custom(Box::new(MongoJsValue(code.into()))),
                &DataType::Text { size: None },
                &ctx(),
            )
            .unwrap();
        assert_eq!(out, Value::Text(code.into()));
    }

    #[test]
    fn convert_to_bytes_returns_utf8_bytes() {
        let code = "function () {}";
        let out = MongoJsType
            .convert(
                Value::Custom(Box::new(MongoJsValue(code.into()))),
                &DataType::Bytes { size: None },
                &ctx(),
            )
            .unwrap();
        assert_eq!(out, Value::Bytes(code.as_bytes().to_vec()));
    }

    #[test]
    fn construct_from_text() {
        let out = MongoJsType
            .construct(
                Value::Text("function () {}".into()),
                &DataType::Text { size: None },
                &ctx(),
            )
            .unwrap();
        match out {
            Value::Custom(v) => assert_eq!(
                v.as_any().downcast_ref::<MongoJsValue>().unwrap().0,
                "function () {}"
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn construct_from_bytes_utf8_ok() {
        let out = MongoJsType
            .construct(
                Value::Bytes(b"function() {}".to_vec()),
                &DataType::Bytes { size: None },
                &ctx(),
            )
            .unwrap();
        match out {
            Value::Custom(v) => assert_eq!(
                v.as_any().downcast_ref::<MongoJsValue>().unwrap().0,
                "function() {}"
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn construct_from_bytes_invalid_utf8_rejected() {
        // 0xff is not valid utf-8.
        let res = MongoJsType.construct(
            Value::Bytes(vec![0xff, 0xfe]),
            &DataType::Bytes { size: None },
            &ctx(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn round_trip_text() {
        let original = MongoJsValue("function () { return null; }".into());
        let encoded = MongoJsType
            .convert(
                Value::Custom(Box::new(original.clone())),
                &DataType::Text { size: None },
                &ctx(),
            )
            .unwrap();
        let decoded = MongoJsType
            .construct(encoded, &DataType::Text { size: None }, &ctx())
            .unwrap();
        match decoded {
            Value::Custom(v) => assert_eq!(
                v.as_any().downcast_ref::<MongoJsValue>().unwrap().0,
                original.0
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn parse_default_accepts_string() {
        let v = MongoJsType
            .parse_default(&toml::Value::String("function () {}".into()))
            .unwrap()
            .expect("Some");
        match v {
            Value::Custom(c) => assert_eq!(
                c.as_any().downcast_ref::<MongoJsValue>().unwrap().0,
                "function () {}"
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn parse_default_rejects_non_string() {
        let res = MongoJsType.parse_default(&toml::Value::Integer(1));
        assert!(matches!(res, Err(DefaultParseError::TypeMismatch { .. })));
    }

    #[test]
    fn matrix_can_convert_to_unbounded_text_and_bytes_only() {
        let t = MongoJsType;
        assert!(t.can_convert_to(&DataType::Text { size: None }, false));
        assert!(t.can_convert_to(&DataType::Bytes { size: None }, false));
        // Bounded sinks rejected — JS code body is unbounded; a
        // bounded sink would silently overflow for any code longer
        // than the declared width.
        assert!(!t.can_convert_to(&DataType::Text { size: Some(10) }, false));
        assert!(!t.can_convert_to(&DataType::Bytes { size: Some(8) }, false));
        assert!(!t.can_convert_to(&DataType::Int32, false));
        assert!(!t.can_convert_to(&DataType::Json, false));
    }

    #[test]
    fn matrix_can_construct_from_text_and_bytes_any_size() {
        let t = MongoJsType;
        assert!(t.can_construct_from(&DataType::Text { size: None }, false));
        assert!(t.can_construct_from(&DataType::Text { size: Some(10) }, false));
        assert!(t.can_construct_from(&DataType::Bytes { size: None }, false));
        assert!(t.can_construct_from(&DataType::Bytes { size: Some(8) }, false));
        assert!(!t.can_construct_from(&DataType::Int32, false));
    }

    #[test]
    fn dyn_value_to_json_emits_code_string() {
        let v = MongoJsValue("function () { return 1; }".into());
        let j = DynValue::to_json(&v).unwrap();
        assert_eq!(j, serde_json::json!("function () { return 1; }"));
    }

    #[test]
    fn dyn_value_eq_dyn() {
        let a: Box<dyn DynValue> = Box::new(MongoJsValue("x".into()));
        let b: Box<dyn DynValue> = Box::new(MongoJsValue("x".into()));
        let c: Box<dyn DynValue> = Box::new(MongoJsValue("y".into()));
        assert!(a.eq_dyn(&*b));
        assert!(!a.eq_dyn(&*c));
    }
}
