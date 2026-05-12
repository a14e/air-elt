//! `Array(T)` — CH native variable-length array.
//!
//! RowBinary layout: `VarUInt len` followed by `len` element payloads,
//! each encoded per the element type.
//!
//! Cross-canonical conversion: `Array(T) ↔ Json`. Elements are recursively
//! converted via the dispatch layer; any conversion that the inner type
//! supports works element-wise.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChArrayType {
    pub element: DataType,
    /// Whether the element type is `Nullable(...)`. Affects RowBinary
    /// encoding: each element gets a 1-byte NULL flag before its payload.
    pub element_nullable: bool,
}

impl ChArrayType {
    pub const KIND: &'static str = "clickhouse.array";
}

impl DynType for ChArrayType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn display(&self) -> String {
        format!("Array({})", self.element)
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        match target {
            DataType::Json => true,
            DataType::Custom(t) if t.kind() == Self::KIND => true,
            _ => false,
        }
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        match src {
            DataType::Json => true,
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
            DataType::Json => {
                let v = unwrap_array_value(value, self)?;
                let elements: Vec<serde_json::Value> = v
                    .elements
                    .into_iter()
                    .map(|elem| crate::types::array::value_to_json(&elem))
                    .collect::<Result<_, _>>()
                    .map_err(|_e| ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(self.clone())),
                        dst: target.clone(),
                    })?;
                Ok(Value::Json(serde_json::Value::Array(elements)))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(self.clone())),
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
            DataType::Json => {
                let arr = match value {
                    Value::Json(j) => j,
                    _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
                };
                let json_array = match arr {
                    serde_json::Value::Array(a) => a,
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch {
                            src: DataType::Json,
                        });
                    }
                };
                let elements: Vec<Value> = json_array.into_iter().map(json_to_value).collect();
                Ok(Value::Custom(Box::new(ChArrayValue {
                    element_type: self.element.clone(),
                    elements,
                })))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(self.clone())),
            }),
        }
    }

    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        Ok(None)
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(self.clone())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChArrayValue {
    pub element_type: DataType,
    pub elements: Vec<Value>,
}

impl DynValue for ChArrayValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChArrayType {
            element: self.element_type.clone(),
            element_nullable: false,
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
            .downcast_ref::<ChArrayValue>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        let elements: Result<Vec<serde_json::Value>, _> =
            self.elements.iter().map(value_to_json).collect();
        Ok(serde_json::Value::Array(elements?))
    }
}

fn unwrap_array_value(value: Value, ty: &ChArrayType) -> Result<ChArrayValue, ConvertError> {
    match value {
        Value::Custom(b) => b
            .into_any()
            .downcast::<ChArrayValue>()
            .map(|v| *v)
            .map_err(|_| ConvertError::Unsupported {
                src: DataType::Custom(Box::new(ty.clone())),
                dst: DataType::Json,
            }),
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ty.clone())),
        }),
    }
}

pub(super) fn value_to_json(v: &Value) -> Result<serde_json::Value, JsonEncodeError> {
    air_elt_core::types::json_encode::value_to_json(v)
}

pub(super) fn json_to_value(j: serde_json::Value) -> Value {
    match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                if i >= i32::MIN as i64 && i <= i32::MAX as i64 {
                    Value::Int32(i as i32)
                } else {
                    Value::Int64(i)
                }
            } else if let Some(f) = n.as_f64() {
                Value::Float64(f)
            } else {
                Value::Text(n.to_string())
            }
        }
        serde_json::Value::String(s) => Value::Text(s),
        serde_json::Value::Array(arr) => {
            let elements: Vec<Value> = arr.into_iter().map(json_to_value).collect();
            Value::Json(serde_json::Value::Array(
                elements
                    .into_iter()
                    .map(|v| match v {
                        Value::Json(j) => j,
                        other => serde_json::Value::String(format!("{other:?}")),
                    })
                    .collect(),
            ))
        }
        serde_json::Value::Object(_) => Value::Json(j),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn array_type_kind() {
        let t = ChArrayType {
            element: DataType::Int32,
            element_nullable: false,
        };
        assert_eq!(t.kind(), "clickhouse.array");
        assert_eq!(t.display(), "Array(int32)");
    }

    #[test]
    fn array_value_to_json() {
        let v = ChArrayValue {
            element_type: DataType::Int32,
            elements: vec![Value::Int32(1), Value::Int32(2)],
        };
        let j = v.to_json().unwrap();
        assert_eq!(j, serde_json::json!([1, 2]));
    }

    #[test]
    fn array_cross_canonical_roundtrip() {
        let ty = ChArrayType {
            element: DataType::Int32,
            element_nullable: false,
        };
        let val = Value::Custom(Box::new(ChArrayValue {
            element_type: DataType::Int32,
            elements: vec![Value::Int32(42)],
        }));
        let ctx = ConversionContext::default();
        let json_val = ty.convert(val.clone(), &DataType::Json, &ctx).unwrap();
        let roundtripped = ty.construct(json_val, &DataType::Json, &ctx).unwrap();
        assert_eq!(val, roundtripped);
    }
}
