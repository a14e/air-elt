//! `Int128` and `UInt128` columns.
//!
//! ClickHouse stores these as 16-byte little-endian two's-complement
//! integers in RowBinary format.
//!
//! The canonical pivot has no 128-bit signed/unsigned variants, so these
//! are represented as `DataType::Custom`. Cross-canonical conversions are
//! allowed both ways via `BigInt` (the widest canonical integer type).

use std::any::Any;

use num_bigint::{BigInt, Sign};

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

// ---------------------------------------------------------------- Int128

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChInt128Type;

impl ChInt128Type {
    pub const KIND: &'static str = "clickhouse.int128";
}

impl DynType for ChInt128Type {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn display(&self) -> String {
        "Int128".to_string()
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::BigInt { .. })
            || matches!(target, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::BigInt { .. })
            || matches!(src, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
            DataType::BigInt { .. } => {
                let v = downcast_int128(value, target)?;
                Ok(Value::BigInt(BigInt::from(v.0)))
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
            DataType::BigInt { .. } => {
                let n = match value {
                    Value::BigInt(b) => b,
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
                    }
                };
                let v = i128::try_from(n).map_err(|_| ConvertError::Unsupported {
                    src: src.clone(),
                    dst: DataType::Custom(Box::new(*self)),
                })?;
                Ok(Value::Custom(Box::new(ChInt128Value(v))))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        let n = literal
            .as_integer()
            .ok_or(DefaultParseError::TypeMismatch {
                dst: DataType::Custom(Box::new(*self)),
            })?;
        Ok(Some(Value::Custom(Box::new(ChInt128Value(i128::from(n))))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChInt128Value(pub i128);

impl DynValue for ChInt128Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChInt128Type)
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
            .downcast_ref::<ChInt128Value>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.to_string()))
    }
}

fn downcast_int128(value: Value, target: &DataType) -> Result<ChInt128Value, ConvertError> {
    match value {
        Value::Custom(b) => {
            let any = b.into_any();
            any.downcast::<ChInt128Value>()
                .map(|v| *v)
                .map_err(|_| ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(ChInt128Type)),
                    dst: target.clone(),
                })
        }
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ChInt128Type)),
        }),
    }
}

// ---------------------------------------------------------------- UInt128

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChUInt128Type;

impl ChUInt128Type {
    pub const KIND: &'static str = "clickhouse.uint128";
}

impl DynType for ChUInt128Type {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn display(&self) -> String {
        "UInt128".to_string()
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::BigInt { .. })
            || matches!(target, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::BigInt { .. })
            || matches!(src, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
            DataType::BigInt { .. } => {
                let v = downcast_uint128(value, target)?;
                Ok(Value::BigInt(BigInt::from(v.0)))
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
            DataType::BigInt { .. } => {
                let n = match value {
                    Value::BigInt(b) => b,
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
                    }
                };
                if n.sign() == Sign::Minus {
                    return Err(ConvertError::Unsupported {
                        src: src.clone(),
                        dst: DataType::Custom(Box::new(*self)),
                    });
                }
                let (_, digits) = n.to_u64_digits();
                // Build u128 from at most 2 u64 limbs (LE order from BigInt).
                let low = digits.first().copied().unwrap_or(0);
                let high = digits.get(1).copied().unwrap_or(0);
                if digits.len() > 2 {
                    return Err(ConvertError::Unsupported {
                        src: src.clone(),
                        dst: DataType::Custom(Box::new(*self)),
                    });
                }
                let v = u128::from(low) | (u128::from(high) << 64);
                Ok(Value::Custom(Box::new(ChUInt128Value(v))))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        let n = literal
            .as_integer()
            .ok_or(DefaultParseError::TypeMismatch {
                dst: DataType::Custom(Box::new(*self)),
            })?;
        let v = u128::try_from(n).map_err(|_| DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(*self)),
        })?;
        Ok(Some(Value::Custom(Box::new(ChUInt128Value(v)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChUInt128Value(pub u128);

impl DynValue for ChUInt128Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChUInt128Type)
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
            .downcast_ref::<ChUInt128Value>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.0.to_string()))
    }
}

fn downcast_uint128(value: Value, target: &DataType) -> Result<ChUInt128Value, ConvertError> {
    match value {
        Value::Custom(b) => {
            let any = b.into_any();
            any.downcast::<ChUInt128Value>()
                .map(|v| *v)
                .map_err(|_| ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(ChUInt128Type)),
                    dst: target.clone(),
                })
        }
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ChUInt128Type)),
        }),
    }
}

/// Extract the raw bytes for the UInt128 constructor test.
pub fn bigint_to_uint128(n: &BigInt) -> Option<u128> {
    if n.sign() == Sign::Minus {
        return None;
    }
    let (_, digits) = n.to_u64_digits();
    if digits.len() > 2 {
        return None;
    }
    let low = digits.first().copied().unwrap_or(0);
    let high = digits.get(1).copied().unwrap_or(0);
    Some(u128::from(low) | (u128::from(high) << 64))
}

/// Encode an `i128` as 16 little-endian bytes.
pub fn i128_to_le16(n: i128) -> [u8; 16] {
    n.to_le_bytes()
}

/// Encode a `u128` as 16 little-endian bytes.
pub fn u128_to_le16(n: u128) -> [u8; 16] {
    n.to_le_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use num_bigint::BigUint;

    use super::*;

    #[test]
    fn int128_le_bytes() {
        // 1 encoded as i128 little-endian is [1, 0, 0, ..., 0]
        let bytes = i128_to_le16(1_i128);
        assert_eq!(bytes[0], 1);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn int128_negative_le_bytes() {
        // -1 in two's complement is all 0xFF bytes.
        let bytes = i128_to_le16(-1_i128);
        assert!(bytes.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn uint128_le_bytes() {
        let bytes = u128_to_le16(1_u128);
        assert_eq!(bytes[0], 1);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn int128_bigint_roundtrip() {
        let original = ChInt128Value(12345678901234567890_i128);
        let as_bigint = BigInt::from(original.0);
        let back = i128::try_from(as_bigint).unwrap();
        assert_eq!(back, original.0);
    }

    #[test]
    fn uint128_bigint_roundtrip() {
        let original = ChUInt128Value(u128::MAX);
        let biguint = BigUint::from(original.0);
        let as_bigint = BigInt::from(biguint);
        let recovered = bigint_to_uint128(&as_bigint).unwrap();
        assert_eq!(recovered, original.0);
    }
}
