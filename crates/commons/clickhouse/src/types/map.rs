//! `Map(K, V)` — CH native key-value map.
//!
//! RowBinary layout: `VarUInt len` (pair count), then `len` pairs of
//! (key-payload, value-payload), each encoded per their respective types.
//!
//! Cross-canonical conversion: `Map(K, V) ↔ Json`. Serialised as an
//! array of `[key, value]` pairs in JSON.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

/// Re-use the JSON helpers from the array module (they live at crate
/// scope).  We import directly rather than via `super::array` to avoid
/// a circular public-module dependency.
use super::array::{json_to_typed_value, value_to_json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChMapType {
    pub key: DataType,
    pub value: DataType,
    /// Whether the key/value types are `Nullable(...)`. Affects RowBinary
    /// encoding: each key/value gets a 1-byte NULL flag before its payload.
    pub key_nullable: bool,
    pub value_nullable: bool,
}

impl ChMapType {
    pub const KIND: &'static str = "clickhouse.map";
}

impl DynType for ChMapType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn display(&self) -> String {
        format!("Map({}, {})", self.key, self.value)
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
                let v = unwrap_map_value(value, self)?;
                let pairs: Vec<serde_json::Value> = v
                    .entries
                    .into_iter()
                    .map(|(k, val)| {
                        let key_json = value_to_json(&k)?;
                        let val_json = value_to_json(&val)?;
                        Ok(serde_json::Value::Array(vec![key_json, val_json]))
                    })
                    .collect::<Result<_, JsonEncodeError>>()
                    .map_err(|_| ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(self.clone())),
                        dst: target.clone(),
                    })?;
                Ok(Value::Json(serde_json::Value::Array(pairs)))
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
                let entries: Vec<(Value, Value)> = json_array
                    .into_iter()
                    .map(|pair| match pair {
                        serde_json::Value::Array(mut a) if a.len() == 2 => {
                            let val = json_to_typed_value(a.remove(1), &self.value);
                            let key = json_to_typed_value(a.remove(0), &self.key);
                            Ok((key, val))
                        }
                        _ => Err(ConvertError::ValueShapeMismatch {
                            src: DataType::Json,
                        }),
                    })
                    .collect::<Result<_, _>>()?;
                Ok(Value::Custom(Box::new(ChMapValue { entries })))
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
pub struct ChMapValue {
    pub entries: Vec<(Value, Value)>,
}

impl DynValue for ChMapValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        // Use a placeholder element type — maps reconstructed from JSON
        // carry no type-level key/value info.
        Box::new(ChMapType {
            key: DataType::Json,
            value: DataType::Json,
            key_nullable: false,
            value_nullable: false,
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
            .downcast_ref::<ChMapValue>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        let pairs: Result<Vec<serde_json::Value>, _> = self
            .entries
            .iter()
            .map(|(k, v)| {
                let key_json = value_to_json(k)?;
                let val_json = value_to_json(v)?;
                Ok(serde_json::Value::Array(vec![key_json, val_json]))
            })
            .collect();
        Ok(serde_json::Value::Array(pairs?))
    }
}

fn unwrap_map_value(value: Value, ty: &ChMapType) -> Result<ChMapValue, ConvertError> {
    match value {
        Value::Custom(b) => b
            .into_any()
            .downcast::<ChMapValue>()
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
    fn map_type_kind() {
        let t = ChMapType {
            key: DataType::Text { size: None },
            value: DataType::Int32,
            key_nullable: false,
            value_nullable: false,
        };
        assert_eq!(t.kind(), "clickhouse.map");
        assert_eq!(t.display(), "Map(text, int32)");
    }

    #[test]
    fn map_value_to_json() {
        let v = ChMapValue {
            entries: vec![
                (Value::Text("a".into()), Value::Int32(1)),
                (Value::Text("b".into()), Value::Int32(2)),
            ],
        };
        let j = v.to_json().unwrap();
        assert_eq!(j, serde_json::json!([["a", 1], ["b", 2]]));
    }
}
