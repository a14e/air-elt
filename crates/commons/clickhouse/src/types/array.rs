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

    fn kind(&self) -> &str {
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
                let elements: Vec<Value> = json_array
                    .into_iter()
                    .map(|j| json_to_typed_value(j, &self.element))
                    .collect::<Result<_, _>>()?;
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

    fn parse_default(&self, _literal: &toml::Value) -> Result<Option<Value>, String> {
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
    fn is_equal(&self, other: &dyn DynValue) -> bool {
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
    // Generic JSON pivot — never overflows, so the result is always Ok.
    // unwrap is safe because the DataType::Json arm in json_to_typed_value
    // only routes through range-checked or text fallbacks.
    json_to_typed_value(j, &DataType::Json).expect("DataType::Json target cannot overflow")
}

/// Convert a JSON value to the canonical `Value` variant dictated by
/// `target`. For `DataType::Json` (the generic pivot) this is the legacy
/// best-effort mapping. For concrete CH types (Int8, UInt64, etc.) the
/// JSON number is narrowed to the exact target width and an out-of-range
/// number is rejected — silent zero-default would corrupt data.
pub(super) fn json_to_typed_value(
    j: serde_json::Value,
    target: &DataType,
) -> Result<Value, ConvertError> {
    fn overflow(target: &DataType) -> ConvertError {
        ConvertError::Overflow {
            dst: target.clone(),
        }
    }

    Ok(match j {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(b) => Value::Bool(b),
        serde_json::Value::Number(n) => match target {
            DataType::Int8 => Value::Int8(
                n.as_i64()
                    .and_then(|i| i8::try_from(i).ok())
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::Int16 => Value::Int16(
                n.as_i64()
                    .and_then(|i| i16::try_from(i).ok())
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::Int32 => Value::Int32(
                n.as_i64()
                    .and_then(|i| i32::try_from(i).ok())
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::Int64 => Value::Int64(n.as_i64().ok_or_else(|| overflow(target))?),
            DataType::UInt8 => Value::UInt8(
                n.as_u64()
                    .and_then(|i| u8::try_from(i).ok())
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::UInt16 => Value::UInt16(
                n.as_u64()
                    .and_then(|i| u16::try_from(i).ok())
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::UInt32 => Value::UInt32(
                n.as_u64()
                    .and_then(|i| u32::try_from(i).ok())
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::UInt64 => Value::UInt64(n.as_u64().ok_or_else(|| overflow(target))?),
            DataType::Float32 => Value::Float32(
                n.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| overflow(target))?,
            ),
            DataType::Float64 => Value::Float64(n.as_f64().ok_or_else(|| overflow(target))?),
            _ => {
                // Fall back to integer/float heuristics for the generic
                // DataType::Json pivot — no width to overflow against.
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
        },
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
    })
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
    fn json_to_typed_value_rejects_int8_overflow() {
        let err = json_to_typed_value(serde_json::json!(300), &DataType::Int8).unwrap_err();
        assert!(matches!(err, ConvertError::Overflow { .. }));
    }

    #[test]
    fn json_to_typed_value_rejects_uint8_overflow() {
        let err = json_to_typed_value(serde_json::json!(500), &DataType::UInt8).unwrap_err();
        assert!(matches!(err, ConvertError::Overflow { .. }));
    }

    #[test]
    fn json_to_typed_value_rejects_uint16_negative() {
        // Negative JSON number does not fit any UInt — n.as_u64() returns None.
        let err = json_to_typed_value(serde_json::json!(-1), &DataType::UInt16).unwrap_err();
        assert!(matches!(err, ConvertError::Overflow { .. }));
    }

    #[test]
    fn json_to_typed_value_accepts_in_range() {
        let v = json_to_typed_value(serde_json::json!(120), &DataType::Int8).unwrap();
        assert_eq!(v, Value::Int8(120));
        let v = json_to_typed_value(serde_json::json!(-1), &DataType::Int8).unwrap();
        assert_eq!(v, Value::Int8(-1));
    }

    #[test]
    fn array_construct_propagates_element_overflow() {
        let ty = ChArrayType {
            element: DataType::Int8,
            element_nullable: false,
        };
        let bad = Value::Json(serde_json::json!([1, 999]));
        let ctx = ConversionContext::default();
        let err = ty.construct(bad, &DataType::Json, &ctx).unwrap_err();
        assert!(matches!(err, ConvertError::Overflow { .. }));
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

    #[test]
    fn array_cross_canonical_preserves_int8_element() {
        let ty = ChArrayType {
            element: DataType::Int8,
            element_nullable: false,
        };
        let val = Value::Custom(Box::new(ChArrayValue {
            element_type: DataType::Int8,
            elements: vec![Value::Int8(7), Value::Int8(-3)],
        }));
        let ctx = ConversionContext::default();
        let json_val = ty.convert(val.clone(), &DataType::Json, &ctx).unwrap();
        let roundtripped = ty.construct(json_val, &DataType::Json, &ctx).unwrap();
        assert_eq!(
            val, roundtripped,
            "Int8 elements must survive JSON roundtrip"
        );
    }

    #[test]
    fn array_cross_canonical_preserves_uint64_element() {
        let ty = ChArrayType {
            element: DataType::UInt64,
            element_nullable: false,
        };
        let val = Value::Custom(Box::new(ChArrayValue {
            element_type: DataType::UInt64,
            elements: vec![Value::UInt64(3_000_000_000)],
        }));
        let ctx = ConversionContext::default();
        let json_val = ty.convert(val.clone(), &DataType::Json, &ctx).unwrap();
        let roundtripped = ty.construct(json_val, &DataType::Json, &ctx).unwrap();
        assert_eq!(
            val, roundtripped,
            "UInt64 elements must survive JSON roundtrip"
        );
    }

    #[test]
    fn array_cross_canonical_preserves_text_elements() {
        let ty = ChArrayType {
            element: DataType::Text { size: None },
            element_nullable: false,
        };
        let val = Value::Custom(Box::new(ChArrayValue {
            element_type: DataType::Text { size: None },
            elements: vec![Value::Text("hello".into()), Value::Text("world".into())],
        }));
        let ctx = ConversionContext::default();
        let json_val = ty.convert(val.clone(), &DataType::Json, &ctx).unwrap();
        let roundtripped = ty.construct(json_val, &DataType::Json, &ctx).unwrap();
        assert_eq!(
            val, roundtripped,
            "Text elements must survive JSON roundtrip"
        );
    }

    #[test]
    fn array_cross_canonical_preserves_bool_elements() {
        let ty = ChArrayType {
            element: DataType::Bool,
            element_nullable: false,
        };
        let val = Value::Custom(Box::new(ChArrayValue {
            element_type: DataType::Bool,
            elements: vec![Value::Bool(true), Value::Bool(false)],
        }));
        let ctx = ConversionContext::default();
        let json_val = ty.convert(val.clone(), &DataType::Json, &ctx).unwrap();
        let roundtripped = ty.construct(json_val, &DataType::Json, &ctx).unwrap();
        assert_eq!(
            val, roundtripped,
            "Bool elements must survive JSON roundtrip"
        );
    }
}
