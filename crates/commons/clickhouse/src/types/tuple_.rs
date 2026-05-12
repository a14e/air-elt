//! `Tuple(T1, T2, ...)` — CH native heterogeneous tuple.
//!
//! RowBinary layout: field payloads concatenated in order, **no** length
//! prefix (unlike Array/Map). Each field is encoded per its own type.
//!
//! Cross-canonical conversion: `Tuple(...) ↔ Json`. Serialised as a
//! JSON array of field values in declaration order.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

use super::array_::{json_to_value, value_to_json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChTupleType {
    pub fields: Vec<DataType>,
}

impl ChTupleType {
    pub const KIND: &'static str = "clickhouse.tuple";
}

impl DynType for ChTupleType {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn display(&self) -> String {
        let inner: Vec<String> = self.fields.iter().map(|f| f.to_string()).collect();
        format!("Tuple({})", inner.join(", "))
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
                let v = unwrap_tuple_value(value, self)?;
                let elements: Vec<serde_json::Value> = v
                    .fields
                    .iter()
                    .map(value_to_json)
                    .collect::<Result<_, _>>()
                    .map_err(|_| ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(ChTupleType {
                            fields: self.fields.clone(),
                        })),
                        dst: target.clone(),
                    })?;
                Ok(Value::Json(serde_json::Value::Array(elements)))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(ChTupleType {
                    fields: self.fields.clone(),
                })),
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
                let fields: Vec<Value> = json_array.into_iter().map(json_to_value).collect();
                Ok(Value::Custom(Box::new(ChTupleValue { fields })))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(ChTupleType {
                    fields: self.fields.clone(),
                })),
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
pub struct ChTupleValue {
    pub fields: Vec<Value>,
}

impl DynValue for ChTupleValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        // Placeholder — tuples reconstructed from JSON have unknown inner
        // types.  The type descriptor carried on the DataType side is the
        // authoritative source.
        Box::new(ChTupleType {
            fields: vec![DataType::Json; self.fields.len()],
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
            .downcast_ref::<ChTupleValue>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        let elements: Result<Vec<serde_json::Value>, _> =
            self.fields.iter().map(value_to_json).collect();
        Ok(serde_json::Value::Array(elements?))
    }
}

fn unwrap_tuple_value(value: Value, ty: &ChTupleType) -> Result<ChTupleValue, ConvertError> {
    match value {
        Value::Custom(b) => b
            .into_any()
            .downcast::<ChTupleValue>()
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tuple_type_kind() {
        let t = ChTupleType {
            fields: vec![DataType::Int32, DataType::Text { size: None }],
        };
        assert_eq!(t.kind(), "clickhouse.tuple");
        assert_eq!(t.display(), "Tuple(int32, text)");
    }

    #[test]
    fn tuple_value_to_json() {
        let v = ChTupleValue {
            fields: vec![Value::Int32(42), Value::Text("hello".into())],
        };
        let j = v.to_json().unwrap();
        assert_eq!(j, serde_json::json!([42, "hello"]));
    }
}
