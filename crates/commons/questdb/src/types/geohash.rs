//! QuestDB `GEOHASH(Nb)` columns. `N` is the bit width (1..=60); the
//! value is a packed integer placed in the low `N` bits of a `u64`. We
//! surface it as a base32-geohash text string in JSON / pg-wire to match
//! QuestDB's own textual presentation.
//!
//! Cross-canonical conversion is intentionally not offered: the bit width
//! is part of the type identity and there is no canonical pivot that
//! carries that metadata. Custom→Custom only.

use std::any::Any;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

/// Base32 alphabet used by QuestDB geohashes ("0123456789bcdefghjkmnpqrstuvwxyz").
const GEOHASH_ALPHABET: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbGeohashType {
    pub bits: u8,
}

impl QuestDbGeohashType {
    pub const KIND: &'static str = "questdb.geohash";
}

impl DynType for QuestDbGeohashType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        Self::KIND
    }

    fn display(&self) -> String {
        format!("GEOHASH({bits}b)", bits = self.bits)
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        matches!(target, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        matches!(src, DataType::Custom(t) if t.kind() == Self::KIND)
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        match target {
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
            DataType::Custom(t) if t.kind() == Self::KIND => Ok(value),
            _ => Err(ConvertError::Unsupported {
                src: src.clone(),
                dst: DataType::Custom(Box::new(*self)),
            }),
        }
    }

    /// Bit width participates in identity — two `GEOHASH(7b)` and
    /// `GEOHASH(12b)` columns are structurally different.
    fn is_equal(&self, other: &dyn DynType) -> bool {
        other
            .as_any()
            .downcast_ref::<QuestDbGeohashType>()
            .is_some_and(|o| o.bits == self.bits)
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestDbGeohashValue {
    pub bits: u8,
    pub value: u64,
}

impl QuestDbGeohashValue {
    /// Render the geohash as a base32 string. Each base32 character
    /// encodes 5 bits, so the textual representation has length
    /// `ceil(bits / 5)`. If `bits` is not a multiple of 5 the final
    /// character represents the high bits left-aligned and the low
    /// `(5 - bits % 5)` bits are zero — same convention QuestDB uses
    /// when surfacing geohashes textually.
    pub fn to_base32(&self) -> String {
        let chars = self.bits.div_ceil(5) as usize;
        if chars == 0 {
            return String::new();
        }
        // Shift the value left so the highest meaningful bit aligns to
        // position `chars * 5 - 1`.
        let total_bits = chars * 5;
        let pad = total_bits as u32 - self.bits as u32;
        let aligned = self.value.checked_shl(pad).unwrap_or(0);
        let mut out = String::with_capacity(chars);
        for i in (0..chars).rev() {
            let shift = (i as u32) * 5;
            let chunk = ((aligned >> shift) & 0x1F) as usize;
            out.push(GEOHASH_ALPHABET[chunk] as char);
        }
        out
    }
}

impl DynValue for QuestDbGeohashValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(QuestDbGeohashType { bits: self.bits })
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
            .downcast_ref::<QuestDbGeohashValue>()
            .is_some_and(|o| self == o)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(*self)
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        Ok(serde_json::Value::String(self.to_base32()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn base32_round_aligned_width() {
        // 10 bits = 2 base32 chars, value 0b00000_11111 = "0z" (lower 5
        // bits decode to 'z' = 31, upper 5 bits to '0' = 0).
        let v = QuestDbGeohashValue {
            bits: 10,
            value: 0b00000_11111,
        };
        assert_eq!(v.to_base32(), "0z");
    }

    #[test]
    fn base32_zero_value() {
        let v = QuestDbGeohashValue { bits: 25, value: 0 };
        // 25 bits = 5 base32 chars, all '0'.
        assert_eq!(v.to_base32(), "00000");
    }
}
