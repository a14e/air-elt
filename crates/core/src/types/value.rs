use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Serialised with a `{ "type": "...", "value": ... }` internal tag so that
/// round-tripping through JSONB (cursor storage) preserves the exact variant.
/// Untagged serde would silently coerce e.g. `Int64(42)` → `Int16(42)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Null,
    Bool(bool),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    UInt8(u8),
    UInt16(u16),
    UInt32(u32),
    UInt64(u64),
    Float32(f32),
    Float64(f64),
    /// Arbitrary-precision integer. Carries a `num_bigint::BigInt` directly,
    /// not a `BigDecimal`, so plain integer pipelines avoid mantissa+scale
    /// arithmetic. Cursor JSON locks to the canonical decimal-string form
    /// (num-bigint's default emits a `[sign, [u32 digits]]` tuple, which
    /// is lossless but unreadable and brittle across versions).
    #[serde(with = "bigint_serde")]
    BigInt(BigInt),
    /// Arbitrary-precision decimal. JSON cursor storage round-trips through
    /// the canonical decimal-string form (BigDecimal's default serde repr is
    /// a JSON number, which f64-truncates; we lock to string instead).
    #[serde(with = "decimal_serde")]
    Decimal(BigDecimal),
    Text(String),
    Bytes(Vec<u8>),
    Date(NaiveDate),
    Timestamp(DateTime<Utc>),
    Uuid(Uuid),
    Json(serde_json::Value),
}

impl Value {
    pub fn is_null(&self) -> bool {
        matches!(self, Value::Null)
    }
}

/// Serialise `BigInt` through its canonical base-10 string form so JSON
/// cursor storage stays human-readable and stable across `num-bigint`
/// version bumps (the default `[sign, [u32]]` tuple repr is brittle).
mod bigint_serde {
    use num_bigint::BigInt;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::str::FromStr;

    pub fn serialize<S>(value: &BigInt, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_str(&value.to_str_radix(10))
    }

    pub fn deserialize<'de, D>(de: D) -> Result<BigInt, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        BigInt::from_str(&s).map_err(serde::de::Error::custom)
    }
}

/// Serialise `BigDecimal` through its canonical decimal-string form so JSON
/// cursor storage doesn't quietly downcast to f64. The default serde impl
/// emits a JSON number, which loses precision past 2^53.
mod decimal_serde {
    use bigdecimal::BigDecimal;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::str::FromStr;

    pub fn serialize<S>(value: &BigDecimal, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ser.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(de: D) -> Result<BigDecimal, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(de)?;
        BigDecimal::from_str(&s).map_err(serde::de::Error::custom)
    }
}
