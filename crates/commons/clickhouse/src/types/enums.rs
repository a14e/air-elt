//! `Enum8(...)` / `Enum16(...)` — named integer variants.
//!
//! Conversion: enum ↔ `Text` (by variant name) both ways. Numeric
//! conversion to `Int16`/`Int32` is intentionally not provided — the
//! integer value is meaningful only with the variant table in hand, so
//! a downstream consumer can't reliably reverse the map. If you want
//! the underlying integer, declare the source as `Int16` directly on
//! the CH side.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChEnum8Type {
    pub variants: Vec<(String, i8)>,
}

impl ChEnum8Type {
    pub const KIND: &'static str = "clickhouse.enum8";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChEnum16Type {
    pub variants: Vec<(String, i16)>,
}

impl ChEnum16Type {
    pub const KIND: &'static str = "clickhouse.enum16";
}

/// Runtime enum carrier.
///
/// We keep BOTH the variant name (for human-facing JSON encoding and
/// cross-canonical `Text` conversion) and the integer ordinal (for
/// RowBinary encoding). RowBinary needs the integer; resolving the
/// table at encode time would require downcasting `&dyn DynType` —
/// which the trait doesn't admit. Carrying the ordinal on the value
/// itself keeps the encoder self-contained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChEnumValue {
    pub name: String,
    pub value: i16,
}

macro_rules! impl_enum_dyn_type {
    ($t:ident, $kind:expr) => {
        impl DynType for $t {
            fn as_any(&self) -> &dyn Any {
                self
            }

            fn kind(&self) -> &str {
                Self::KIND
            }

            fn display(&self) -> String {
                let names: Vec<String> =
                    self.variants.iter().map(|(n, _)| n.clone()).collect();
                format!("{}({})", $kind, names.join(", "))
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
                        let s = match value {
                            Value::Custom(b) => b,
                            _ => {
                                return Err(ConvertError::ValueShapeMismatch {
                                    src: DataType::Custom(Box::new(self.clone())),
                                });
                            }
                        };
                        let cast = s.into_any().downcast::<ChEnumValue>().map_err(|_| {
                            ConvertError::Unsupported {
                                src: DataType::Custom(Box::new(self.clone())),
                                dst: target.clone(),
                            }
                        })?;
                        Ok(Value::Text(cast.name.clone()))
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
                    DataType::Text { .. } => {
                        let s = match value {
                            Value::Text(t) => t,
                            _ => {
                                return Err(ConvertError::ValueShapeMismatch {
                                    src: src.clone(),
                                });
                            }
                        };
                        let ordinal = self
                            .variants
                            .iter()
                            .find(|(n, _)| n == &s)
                            .map(|(_, v)| i16::from(*v))
                            .ok_or_else(|| ConvertError::Unsupported {
                                src: src.clone(),
                                dst: DataType::Custom(Box::new(self.clone())),
                            })?;
                        Ok(Value::Custom(Box::new(ChEnumValue {
                            name: s,
                            value: ordinal,
                        })))
                    }
                    DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
                    _ => Err(ConvertError::Unsupported {
                        src: src.clone(),
                        dst: DataType::Custom(Box::new(self.clone())),
                    }),
                }
            }

            fn parse_default(
                &self,
                literal: &toml::Value,
            ) -> Result<Option<Value>, DefaultParseError> {
                let s = literal
                    .as_str()
                    .ok_or(DefaultParseError::TypeMismatch {
                        dst: DataType::Custom(Box::new(self.clone())),
                    })?;
                let ordinal = self
                    .variants
                    .iter()
                    .find(|(n, _)| n == s)
                    .map(|(_, v)| i16::from(*v))
                    .ok_or_else(|| DefaultParseError::TypeMismatch {
                        dst: DataType::Custom(Box::new(self.clone())),
                    })?;
                Ok(Some(Value::Custom(Box::new(ChEnumValue {
                    name: s.to_string(),
                    value: ordinal,
                }))))
            }

            fn clone_box(&self) -> Box<dyn DynType> {
                Box::new(self.clone())
            }
        }
    };
}

impl_enum_dyn_type!(ChEnum8Type, "Enum8");
impl_enum_dyn_type!(ChEnum16Type, "Enum16");

impl DynValue for ChEnumValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        // The runtime carrier has no parent table — return a "shape"
        // descriptor with the single observed variant. The matrix uses
        // `kind()` only, so this is fine in practice.
        // Runtime carrier has no parent table — emit a synthetic
        // single-variant descriptor. The matrix uses `kind()` only, so
        // the placeholder ordinal does not affect equality.
        let placeholder = i8::try_from(self.value).unwrap_or(0);
        Box::new(ChEnum8Type {
            variants: vec![(self.name.clone(), placeholder)],
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
            .downcast_ref::<ChEnumValue>()
            .is_some_and(|o| self == o)
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.name.clone()))
    }
}
