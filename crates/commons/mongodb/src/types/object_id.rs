//! `mongodb.object_id` custom type.
//!
//! BSON `ObjectId` is a 12-byte identifier; the first 4 bytes are a
//! big-endian unix timestamp (seconds), the next 5 are a per-process
//! random value, and the last 3 are a counter. Round-tripping through
//! the canonical `Bytes(12)` representation works but loses the ability
//! to write the value back as `Bson::ObjectId` on the sink, so we wire
//! it as a custom type instead.
//!
//! ## Conversion matrix
//!
//! Outbound (`MongoObjectIdType -> canonical`):
//! - `Bytes { size: None | Some(>=12) }` lossless (raw 12-byte payload).
//! - `Text { size: None | Some(>=24) }` lossless (24-char lowercase hex).
//! - `Timestamp` under `truncate=true` (extracts the embedded unix
//!   seconds, sets sub-second to zero — the random + counter part is
//!   dropped).
//! - `Date` under `truncate=true` (timestamp's `date_naive()`).
//!
//! Inbound (`canonical -> MongoObjectIdType`):
//! - `Bytes { size: None | Some(>=12) }` — runtime validates exact len 12.
//! - `Text { size: None | Some(>=24) }` — runtime validates 24-char hex.
//!
//! ## Cursor
//!
//! `can_be_cursor() = true`. ObjectIds carry monotonically-incrementing
//! counters within the same process and timestamps across processes —
//! good enough for cursor ordering in practice.

use std::any::Any;

use bson::oid::ObjectId;
use chrono::{TimeZone, Utc};

use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::default_value::DefaultParseError;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

/// Schema-side descriptor for `mongodb.object_id`.
#[derive(Debug, Clone, Copy)]
pub struct MongoObjectIdType;

/// Runtime carrier for an ObjectId value (12 raw bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MongoObjectIdValue(pub [u8; 12]);

impl MongoObjectIdValue {
    /// Construct from the driver's `ObjectId`.
    pub fn from_oid(oid: ObjectId) -> Self {
        MongoObjectIdValue(oid.bytes())
    }

    /// Convert to the driver's `ObjectId`.
    pub fn to_oid(&self) -> ObjectId {
        ObjectId::from_bytes(self.0)
    }

    /// Lowercase hex (24 chars).
    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(24);
        for b in &self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

impl MongoObjectIdType {
    /// Single source of truth for the kind string. Sites that need to
    /// recognise an ObjectId `DataType::Custom(t)` should compare
    /// against this constant rather than re-spelling the literal.
    pub const KIND: &'static str = "mongodb.object_id";
}

impl DynType for MongoObjectIdType {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        true
    }

    fn can_convert_to(&self, target: &DataType, truncate: bool) -> bool {
        // Sink-column matrix: dispatcher emits exactly 12 bytes / 24
        // hex chars. The sink's declared length must hold or be
        // unbounded. Anything narrower is a guaranteed runtime
        // overflow; anything wider than the canonical width would
        // accept the value but later round-trips might assume the
        // canonical length, so we reject mismatches at validation
        // time.
        match target {
            DataType::Bytes { size } => size.is_none_or(|n| n == 12),
            DataType::Text { size } => size.is_none_or(|n| n == 24),
            DataType::Timestamp | DataType::Date => truncate,
            _ => false,
        }
    }

    fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
        // Source-column matrix: every row must be exactly 12 bytes /
        // 24 hex chars at runtime. A wider source declaration (e.g.
        // `bytea` columns of arbitrary length) cannot guarantee
        // every row will match. Validation rejects the structurally
        // mismatched cases up front; only `Bytes(12)` / `Text(24)` /
        // unbounded (operator's explicit "trust me" opt-in) reach
        // the runtime hex-/length-check.
        match src {
            DataType::Bytes { size } => size.is_none_or(|n| n == 12),
            DataType::Text { size } => size.is_none_or(|n| n == 24),
            _ => false,
        }
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        let oid = unwrap_object_id(&value)?;
        match target {
            DataType::Bytes { .. } => Ok(Value::Bytes(oid.0.to_vec())),
            DataType::Text { .. } => {
                // Lowercase hex per the BSON spec (`ObjectId::to_hex`).
                Ok(Value::Text(MongoObjectIdValue(oid.0).to_hex()))
            }
            DataType::Timestamp => {
                let secs_be = [oid.0[0], oid.0[1], oid.0[2], oid.0[3]];
                let secs = i64::from(u32::from_be_bytes(secs_be));
                let ts = Utc.timestamp_opt(secs, 0).single().ok_or_else(|| {
                    ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(MongoObjectIdType)),
                        dst: DataType::Timestamp,
                    }
                })?;
                Ok(Value::Timestamp(ts))
            }
            DataType::Date => {
                let secs_be = [oid.0[0], oid.0[1], oid.0[2], oid.0[3]];
                let secs = i64::from(u32::from_be_bytes(secs_be));
                let ts = Utc.timestamp_opt(secs, 0).single().ok_or_else(|| {
                    ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(MongoObjectIdType)),
                        dst: DataType::Date,
                    }
                })?;
                Ok(Value::Date(ts.date_naive()))
            }
            other => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(MongoObjectIdType)),
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
            (Value::Bytes(b), DataType::Bytes { .. }) => {
                if b.len() != 12 {
                    return Err(ConvertError::Unsupported {
                        src: DataType::Bytes { size: None },
                        dst: DataType::Custom(Box::new(MongoObjectIdType)),
                    });
                }
                let mut arr = [0_u8; 12];
                arr.copy_from_slice(&b);
                Ok(Value::Custom(Box::new(MongoObjectIdValue(arr))))
            }
            (Value::Text(s), DataType::Text { .. }) => {
                let arr = parse_hex_24(&s).ok_or_else(|| ConvertError::Unsupported {
                    src: DataType::Text { size: None },
                    dst: DataType::Custom(Box::new(MongoObjectIdType)),
                })?;
                Ok(Value::Custom(Box::new(MongoObjectIdValue(arr))))
            }
            (_, other) => Err(ConvertError::Unsupported {
                src: other.clone(),
                dst: DataType::Custom(Box::new(MongoObjectIdType)),
            }),
        }
    }

    fn parse_default(&self, literal: &toml::Value) -> Result<Option<Value>, DefaultParseError> {
        let s = literal.as_str().ok_or(DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(MongoObjectIdType)),
        })?;
        let arr = parse_hex_24(s).ok_or(DefaultParseError::TypeMismatch {
            dst: DataType::Custom(Box::new(MongoObjectIdType)),
        })?;
        Ok(Some(Value::Custom(Box::new(MongoObjectIdValue(arr)))))
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

impl DynValue for MongoObjectIdValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(MongoObjectIdType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        other
            .as_any()
            .downcast_ref::<MongoObjectIdValue>()
            .map(|o| o.0 == self.0)
            .unwrap_or(false)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }
}

fn unwrap_object_id(v: &Value) -> Result<&MongoObjectIdValue, ConvertError> {
    match v {
        Value::Custom(inner) => inner
            .as_any()
            .downcast_ref::<MongoObjectIdValue>()
            .ok_or_else(|| ConvertError::ValueShapeMismatch {
                src: DataType::Custom(Box::new(MongoObjectIdType)),
            }),
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(MongoObjectIdType)),
        }),
    }
}

fn parse_hex_24(s: &str) -> Option<[u8; 12]> {
    if s.len() != 24 {
        return None;
    }
    let mut out = [0_u8; 12];
    let bytes = s.as_bytes();
    for i in 0..12 {
        let hi = hex_nibble(bytes[2 * i])?;
        let lo = hex_nibble(bytes[2 * i + 1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn ctx() -> ConversionContext {
        ConversionContext::passthrough()
    }
    fn truncate() -> ConversionContext {
        ConversionContext::passthrough().with_truncate()
    }

    fn sample_oid() -> [u8; 12] {
        [
            0x65, 0x4f, 0x10, 0x80, // ts = 0x654f1080 = 1699689600 (2023-11-11T08:00:00Z)
            0x01, 0x02, 0x03, 0x04, 0x05, // random
            0x00, 0x00, 0x01, // counter
        ]
    }

    #[test]
    fn kind_is_stable() {
        assert_eq!(MongoObjectIdType.kind(), "mongodb.object_id");
    }

    #[test]
    fn can_be_cursor_true() {
        assert!(MongoObjectIdType.can_be_cursor());
    }

    #[test]
    fn convert_to_text_yields_24_char_lowercase_hex() {
        let v = Value::Custom(Box::new(MongoObjectIdValue(sample_oid())));
        let out = MongoObjectIdType
            .convert(v, &DataType::Text { size: None }, &ctx())
            .unwrap();
        match out {
            Value::Text(s) => {
                assert_eq!(s.len(), 24);
                assert!(
                    s.chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                );
                assert_eq!(s, "654f10800102030405000001");
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn convert_to_bytes_yields_12_bytes() {
        let v = Value::Custom(Box::new(MongoObjectIdValue(sample_oid())));
        let out = MongoObjectIdType
            .convert(v, &DataType::Bytes { size: None }, &ctx())
            .unwrap();
        match out {
            Value::Bytes(b) => assert_eq!(b, sample_oid().to_vec()),
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn convert_to_timestamp_extracts_seconds() {
        let v = Value::Custom(Box::new(MongoObjectIdValue(sample_oid())));
        let out = MongoObjectIdType
            .convert(v, &DataType::Timestamp, &truncate())
            .unwrap();
        match out {
            Value::Timestamp(ts) => {
                assert_eq!(ts.timestamp(), 0x654f1080);
                assert_eq!(ts.timestamp_subsec_nanos(), 0);
            }
            other => panic!("expected Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn convert_to_date_uses_timestamp_date_naive() {
        let v = Value::Custom(Box::new(MongoObjectIdValue(sample_oid())));
        let out = MongoObjectIdType
            .convert(v, &DataType::Date, &truncate())
            .unwrap();
        match out {
            Value::Date(d) => {
                let expected = Utc
                    .timestamp_opt(0x654f1080, 0)
                    .single()
                    .unwrap()
                    .date_naive();
                assert_eq!(d, expected);
            }
            other => panic!("expected Date, got {other:?}"),
        }
    }

    #[test]
    fn construct_from_bytes_validates_length_12() {
        let out = MongoObjectIdType
            .construct(
                Value::Bytes(sample_oid().to_vec()),
                &DataType::Bytes { size: None },
                &ctx(),
            )
            .unwrap();
        match out {
            Value::Custom(v) => {
                let inner = v
                    .as_any()
                    .downcast_ref::<MongoObjectIdValue>()
                    .expect("downcast");
                assert_eq!(inner.0, sample_oid());
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn construct_from_bytes_wrong_length_rejected() {
        let res = MongoObjectIdType.construct(
            Value::Bytes(vec![0_u8; 11]),
            &DataType::Bytes { size: None },
            &ctx(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn construct_from_text_24_char_hex() {
        let out = MongoObjectIdType
            .construct(
                Value::Text("654f10800102030405000001".into()),
                &DataType::Text { size: None },
                &ctx(),
            )
            .unwrap();
        match out {
            Value::Custom(v) => {
                let inner = v
                    .as_any()
                    .downcast_ref::<MongoObjectIdValue>()
                    .expect("downcast");
                assert_eq!(inner.0, sample_oid());
            }
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn construct_from_text_wrong_length_rejected() {
        let res = MongoObjectIdType.construct(
            Value::Text("deadbeef".into()),
            &DataType::Text { size: None },
            &ctx(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn construct_from_text_non_hex_rejected() {
        let res = MongoObjectIdType.construct(
            Value::Text("zzzzzzzzzzzzzzzzzzzzzzzz".into()),
            &DataType::Text { size: None },
            &ctx(),
        );
        assert!(matches!(res, Err(ConvertError::Unsupported { .. })));
    }

    #[test]
    fn round_trip_bytes() {
        let original = MongoObjectIdValue(sample_oid());
        let encoded = MongoObjectIdType
            .convert(
                Value::Custom(Box::new(original.clone())),
                &DataType::Bytes { size: None },
                &ctx(),
            )
            .unwrap();
        let decoded = MongoObjectIdType
            .construct(encoded, &DataType::Bytes { size: None }, &ctx())
            .unwrap();
        match decoded {
            Value::Custom(v) => assert_eq!(
                v.as_any().downcast_ref::<MongoObjectIdValue>().unwrap().0,
                original.0
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn round_trip_hex() {
        let original = MongoObjectIdValue(sample_oid());
        let encoded = MongoObjectIdType
            .convert(
                Value::Custom(Box::new(original.clone())),
                &DataType::Text { size: None },
                &ctx(),
            )
            .unwrap();
        let decoded = MongoObjectIdType
            .construct(encoded, &DataType::Text { size: None }, &ctx())
            .unwrap();
        match decoded {
            Value::Custom(v) => assert_eq!(
                v.as_any().downcast_ref::<MongoObjectIdValue>().unwrap().0,
                original.0
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn parse_default_accepts_24_char_hex() {
        let v = MongoObjectIdType
            .parse_default(&toml::Value::String("654f10800102030405000001".into()))
            .unwrap()
            .expect("Some");
        match v {
            Value::Custom(c) => assert_eq!(
                c.as_any().downcast_ref::<MongoObjectIdValue>().unwrap().0,
                sample_oid()
            ),
            other => panic!("expected Custom, got {other:?}"),
        }
    }

    #[test]
    fn parse_default_rejects_non_string() {
        let res = MongoObjectIdType.parse_default(&toml::Value::Integer(42));
        assert!(matches!(res, Err(DefaultParseError::TypeMismatch { .. })));
    }

    #[test]
    fn parse_default_rejects_short_hex() {
        let res = MongoObjectIdType.parse_default(&toml::Value::String("dead".into()));
        assert!(matches!(res, Err(DefaultParseError::TypeMismatch { .. })));
    }

    #[test]
    fn matrix_can_convert_to_coverage() {
        let t = MongoObjectIdType;
        // Bytes
        assert!(t.can_convert_to(&DataType::Bytes { size: None }, false));
        assert!(t.can_convert_to(&DataType::Bytes { size: Some(12) }, false));
        // Wider sink rejected: dispatcher emits exactly 12 bytes, a
        // wider declared sink would mask future-mismatch.
        assert!(!t.can_convert_to(&DataType::Bytes { size: Some(16) }, false));
        assert!(!t.can_convert_to(&DataType::Bytes { size: Some(11) }, false));
        // Text
        assert!(t.can_convert_to(&DataType::Text { size: None }, false));
        assert!(t.can_convert_to(&DataType::Text { size: Some(24) }, false));
        // Wider sink rejected for the same reason.
        assert!(!t.can_convert_to(&DataType::Text { size: Some(36) }, false));
        assert!(!t.can_convert_to(&DataType::Text { size: Some(23) }, false));
        // Timestamp/Date only with truncate.
        assert!(!t.can_convert_to(&DataType::Timestamp, false));
        assert!(t.can_convert_to(&DataType::Timestamp, true));
        assert!(!t.can_convert_to(&DataType::Date, false));
        assert!(t.can_convert_to(&DataType::Date, true));
        // Unsupported targets.
        assert!(!t.can_convert_to(&DataType::Int32, false));
        assert!(!t.can_convert_to(&DataType::Json, false));
    }

    #[test]
    fn matrix_can_construct_from_coverage() {
        let t = MongoObjectIdType;
        assert!(t.can_construct_from(&DataType::Bytes { size: None }, false));
        assert!(t.can_construct_from(&DataType::Bytes { size: Some(12) }, false));
        assert!(!t.can_construct_from(&DataType::Bytes { size: Some(8) }, false));
        assert!(t.can_construct_from(&DataType::Text { size: None }, false));
        assert!(t.can_construct_from(&DataType::Text { size: Some(24) }, false));
        assert!(!t.can_construct_from(&DataType::Text { size: Some(20) }, false));
        assert!(!t.can_construct_from(&DataType::Int32, false));
        assert!(!t.can_construct_from(&DataType::Timestamp, false));
    }

    #[test]
    fn dyn_value_eq_dyn_compares_bytes() {
        let a: Box<dyn DynValue> = Box::new(MongoObjectIdValue(sample_oid()));
        let b: Box<dyn DynValue> = Box::new(MongoObjectIdValue(sample_oid()));
        let mut c_bytes = sample_oid();
        c_bytes[0] ^= 0xff;
        let c: Box<dyn DynValue> = Box::new(MongoObjectIdValue(c_bytes));
        assert!(a.eq_dyn(&*b));
        assert!(!a.eq_dyn(&*c));
    }

    #[test]
    fn dyn_value_clone_box_preserves_payload() {
        let v: Box<dyn DynValue> = Box::new(MongoObjectIdValue(sample_oid()));
        let cloned = v.clone_box();
        assert!(v.eq_dyn(&*cloned));
    }
}
