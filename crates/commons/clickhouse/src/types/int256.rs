//! `Int256` and `UInt256` columns.
//!
//! ClickHouse stores these as 32-byte little-endian two's-complement
//! integers in RowBinary format.
//!
//! The canonical pivot has no 256-bit variants, so these are represented
//! as `DataType::Custom`. Cross-canonical conversions are allowed via
//! `BigInt` (the widest canonical integer type).

use std::any::Any;

use num_bigint::{BigInt, BigUint, Sign};

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

// ---------------------------------------------------------------- Int256

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChInt256Type;

impl ChInt256Type {
    pub const KIND: &'static str = "clickhouse.int256";
}

impl DynType for ChInt256Type {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn display(&self) -> String {
        "Int256".to_string()
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
                let v = downcast_int256(value, target)?;
                Ok(Value::BigInt(le32_to_bigint(&v.le_bytes)))
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
            DataType::BigInt { .. } => {
                let n = match value {
                    Value::BigInt(b) => b,
                    _ => {
                        return Err(ConvertError::ValueShapeMismatch { src: src.clone() });
                    }
                };
                let le_bytes = bigint_to_le32(&n).ok_or_else(|| ConvertError::Overflow {
                    dst: DataType::Custom(Box::new(self.clone())),
                })?;
                Ok(Value::Custom(Box::new(ChInt256Value { le_bytes })))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(self.clone())),
            }),
        }
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(self.clone())
    }
}

/// Runtime carrier for an Int256 value — 32 bytes in little-endian
/// two's-complement form (same layout as CH RowBinary).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChInt256Value {
    pub le_bytes: [u8; 32],
}

impl DynValue for ChInt256Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChInt256Type)
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
            .downcast_ref::<ChInt256Value>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        let n = le32_to_bigint(&self.le_bytes);
        Ok(serde_json::Value::String(n.to_string()))
    }
}

fn downcast_int256(value: Value, target: &DataType) -> Result<ChInt256Value, ConvertError> {
    match value {
        Value::Custom(b) => {
            let any = b.into_any();
            any.downcast::<ChInt256Value>()
                .map(|v| *v)
                .map_err(|_| ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(ChInt256Type)),
                    dst: target.clone(),
                })
        }
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ChInt256Type)),
        }),
    }
}

// ---------------------------------------------------------------- UInt256

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChUInt256Type;

impl ChUInt256Type {
    pub const KIND: &'static str = "clickhouse.uint256";
}

impl DynType for ChUInt256Type {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn display(&self) -> String {
        "UInt256".to_string()
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
                let v = downcast_uint256(value, target)?;
                let biguint = BigUint::from_bytes_le(&v.le_bytes);
                Ok(Value::BigInt(BigInt::from(biguint)))
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
                        dst: DataType::Custom(Box::new(self.clone())),
                    });
                }
                let le_bytes =
                    biguint_to_le32(n.magnitude()).ok_or_else(|| ConvertError::Overflow {
                        dst: DataType::Custom(Box::new(self.clone())),
                    })?;
                Ok(Value::Custom(Box::new(ChUInt256Value { le_bytes })))
            }
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(self.clone())),
            }),
        }
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(self.clone())
    }
}

/// Runtime carrier for a UInt256 value — 32 bytes in little-endian form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChUInt256Value {
    pub le_bytes: [u8; 32],
}

impl DynValue for ChUInt256Value {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(ChUInt256Type)
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
            .downcast_ref::<ChUInt256Value>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        let biguint = BigUint::from_bytes_le(&self.le_bytes);
        Ok(serde_json::Value::String(biguint.to_string()))
    }
}

fn downcast_uint256(value: Value, target: &DataType) -> Result<ChUInt256Value, ConvertError> {
    match value {
        Value::Custom(b) => {
            let any = b.into_any();
            any.downcast::<ChUInt256Value>()
                .map(|v| *v)
                .map_err(|_| ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(ChUInt256Type)),
                    dst: target.clone(),
                })
        }
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(ChUInt256Type)),
        }),
    }
}

// ---------------------------------------------------------------- helpers

/// Convert a signed `BigInt` to 32-byte LE two's-complement.
///
/// Returns `None` if the value does not fit in 256 bits — i.e. outside
/// the inclusive range `[-2^255, 2^255 - 1]`.
pub fn bigint_to_le32(n: &BigInt) -> Option<[u8; 32]> {
    // `to_signed_bytes_le` returns the minimal-length two's-complement
    // little-endian representation. We then sign-extend (or zero-extend)
    // to exactly 32 bytes.
    let bytes = n.to_signed_bytes_le();
    if bytes.len() > 32 {
        return None;
    }
    let fill = if n.sign() == Sign::Minus { 0xFF } else { 0x00 };
    let mut buf = [fill; 32];
    buf[..bytes.len()].copy_from_slice(&bytes);
    Some(buf)
}

/// Convert a 32-byte LE two's-complement array back to a signed `BigInt`.
pub fn le32_to_bigint(bytes: &[u8; 32]) -> BigInt {
    // Sign bit is the MSB of the last byte.
    let is_negative = bytes[31] & 0x80 != 0;
    if is_negative {
        // Compute: -(NOT(x) + 1) = -(bitwise-NOT then add 1)
        let mut inverted = *bytes;
        for b in &mut inverted {
            *b = !*b;
        }
        let magnitude = BigUint::from_bytes_le(&inverted) + 1u32;
        BigInt::from_biguint(Sign::Minus, magnitude)
    } else {
        let magnitude = BigUint::from_bytes_le(bytes);
        BigInt::from_biguint(Sign::Plus, magnitude)
    }
}

/// Convert a `BigUint` to 32-byte LE (unsigned).
///
/// Returns `None` if the value does not fit in 256 bits — i.e. greater
/// than or equal to `2^256`.
pub fn biguint_to_le32(n: &BigUint) -> Option<[u8; 32]> {
    let bytes = n.to_bytes_le();
    if bytes.len() > 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf[..bytes.len()].copy_from_slice(&bytes);
    Some(buf)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bigint_to_le32_positive() {
        let n = BigInt::from(1i64);
        let bytes = bigint_to_le32(&n).unwrap();
        assert_eq!(bytes[0], 1);
        assert!(bytes[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn bigint_to_le32_negative_one() {
        let n = BigInt::from(-1i64);
        let bytes = bigint_to_le32(&n).unwrap();
        // -1 in two's complement is all 0xFF.
        assert!(bytes.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn le32_to_bigint_roundtrip_positive() {
        let original = BigInt::from(123456789_i64);
        let bytes = bigint_to_le32(&original).unwrap();
        let back = le32_to_bigint(&bytes);
        assert_eq!(back, original);
    }

    #[test]
    fn le32_to_bigint_roundtrip_negative() {
        let original = BigInt::from(-987654321_i64);
        let bytes = bigint_to_le32(&original).unwrap();
        let back = le32_to_bigint(&bytes);
        assert_eq!(back, original);
    }

    #[test]
    fn biguint_to_le32_round_trip() {
        let n = BigUint::from(u128::MAX);
        let bytes = biguint_to_le32(&n).unwrap();
        let back = BigUint::from_bytes_le(&bytes);
        assert_eq!(back, n);
    }

    fn pow2_bigint(exp: usize) -> BigInt {
        BigInt::from(1u32) << exp
    }

    fn pow2_biguint(exp: usize) -> BigUint {
        BigUint::from(1u32) << exp
    }

    #[test]
    fn bigint_to_le32_int256_max_fits() {
        // 2^255 - 1 = largest valid Int256.
        let max = pow2_bigint(255) - 1u32;
        let bytes = bigint_to_le32(&max).unwrap();
        assert_eq!(le32_to_bigint(&bytes), max);
    }

    #[test]
    fn bigint_to_le32_int256_min_fits() {
        // -2^255 = smallest valid Int256.
        let min = -pow2_bigint(255);
        let bytes = bigint_to_le32(&min).unwrap();
        assert_eq!(le32_to_bigint(&bytes), min);
    }

    #[test]
    fn bigint_to_le32_positive_overflow() {
        // 2^255 does not fit as a signed 256-bit integer.
        let too_big = pow2_bigint(255);
        assert!(bigint_to_le32(&too_big).is_none());
    }

    #[test]
    fn bigint_to_le32_negative_overflow() {
        // -2^255 - 1 does not fit.
        let too_small = -pow2_bigint(255) - 1u32;
        assert!(bigint_to_le32(&too_small).is_none());
    }

    #[test]
    fn biguint_to_le32_uint256_max_fits() {
        // 2^256 - 1 = largest valid UInt256.
        let max = pow2_biguint(256) - 1u32;
        let bytes = biguint_to_le32(&max).unwrap();
        assert_eq!(BigUint::from_bytes_le(&bytes), max);
    }

    #[test]
    fn biguint_to_le32_overflow() {
        // 2^256 does not fit.
        let too_big = pow2_biguint(256);
        assert!(biguint_to_le32(&too_big).is_none());
    }

    #[test]
    fn construct_int256_overflow_errors() {
        let ctx = ConversionContext::passthrough();
        let too_big = pow2_bigint(255);
        let err = ChInt256Type
            .construct(
                Value::BigInt(too_big),
                &DataType::BigInt { width: None },
                &ctx,
            )
            .unwrap_err();
        assert!(matches!(err, ConvertError::Overflow { .. }));
    }

    #[test]
    fn construct_uint256_overflow_errors() {
        let ctx = ConversionContext::passthrough();
        let too_big = BigInt::from(pow2_biguint(256));
        let err = ChUInt256Type
            .construct(
                Value::BigInt(too_big),
                &DataType::BigInt { width: None },
                &ctx,
            )
            .unwrap_err();
        assert!(matches!(err, ConvertError::Overflow { .. }));
    }
}
