//! Bidirectional codec between BSON and the canonical `Value` /
//! `DataType` model.
//!
//! Mongo is schemaless, so this codec also produces a best-effort
//! `DataType` for any BSON value — the inference module
//! (`super::infer`) folds those per-field to derive a sampled schema.
//! Conversion is intentionally narrow: we map the small set of BSON
//! types that have lossless canonical equivalents and reject the rest
//! at runtime.
//!
//! Known precision/representation caveats (acceptable for MVP):
//! - **Sub-millisecond truncation on `Timestamp`**: BSON `DateTime` is
//!   millisecond-resolution, so chrono nanoseconds are dropped on the
//!   way out. Identity round-trips of typical pipeline timestamps are
//!   millisecond-faithful; sub-ms data will silently floor.
//! - **Nested Document/Array → `Value::Json`**: implemented via
//!   `bson::from_bson` into a `serde_json::Value`. Nested ObjectIds,
//!   Decimal128, Binary and DateTime values are serialised as the
//!   driver's extended-JSON shapes inside the JSON tree. They are not
//!   lost, but consumers reading `Value::Json` see a JSON
//!   representation rather than the original BSON variants. If a
//!   pipeline needs full fidelity for nested BSON it should map the
//!   leaf paths (`addr.city`, `addr.geo.lat`) explicitly rather than
//!   pulling the whole `addr` subdocument as JSON.

use bigdecimal::BigDecimal;
use bson::{Bson, Decimal128};
use chrono::{TimeZone, Utc};
use serde_json::Value as JsonValue;
use std::str::FromStr;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::types::{DataType, Value};

/// Decode a `Bson` value into the canonical `Value`. Returns an
/// error for BSON types we cannot represent losslessly (regex,
/// JavaScript code, DBPointer, MinKey, MaxKey, etc.).
pub fn from_bson(b: &Bson) -> RuntimeResult<Value> {
    Ok(match b {
        Bson::Null => Value::Null,
        Bson::Boolean(v) => Value::Bool(*v),
        Bson::Int32(v) => Value::Int32(*v),
        Bson::Int64(v) => Value::Int64(*v),
        Bson::Double(v) => Value::Float64(*v),
        Bson::String(s) => Value::Text(s.clone()),
        Bson::Binary(b) => Value::Bytes(b.bytes.clone()),
        Bson::ObjectId(oid) => Value::Bytes(oid.bytes().to_vec()),
        Bson::DateTime(dt) => {
            let millis = dt.timestamp_millis();
            let secs = millis.div_euclid(1000);
            let nanos = (millis.rem_euclid(1000) as u32) * 1_000_000;
            let ts = Utc
                .timestamp_opt(secs, nanos)
                .single()
                .ok_or_else(|| RuntimeError::Other("invalid BSON DateTime".into()))?;
            Value::Timestamp(ts)
        }
        Bson::Decimal128(d) => {
            let parsed = BigDecimal::from_str(&decimal_to_string(d))
                .map_err(|e| RuntimeError::Other(format!("decimal128 parse: {e}")))?;
            Value::Decimal(parsed)
        }
        Bson::Document(_) | Bson::Array(_) => {
            let json: JsonValue = bson::from_bson(b.clone()).map_err(RuntimeError::backend)?;
            Value::Json(json)
        }
        other => {
            return Err(RuntimeError::Other(format!(
                "unsupported BSON variant: {:?}",
                other.element_type()
            )));
        }
    })
}

/// Encode a canonical `Value` as BSON. Lossless for the subset that
/// `from_bson` accepts; bigint/decimal go through their string
/// representation when they overflow Decimal128 (Mongo's native
/// upper bound).
pub fn to_bson(v: &Value) -> RuntimeResult<Bson> {
    Ok(match v {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int16(n) => Bson::Int32(i32::from(*n)),
        Value::Int32(n) => Bson::Int32(*n),
        Value::Int64(n) => Bson::Int64(*n),
        Value::UInt8(n) => Bson::Int32(i32::from(*n)),
        Value::UInt16(n) => Bson::Int32(i32::from(*n)),
        Value::UInt32(n) => Bson::Int64(i64::from(*n)),
        Value::UInt64(n) => {
            if let Ok(as_i64) = i64::try_from(*n) {
                Bson::Int64(as_i64)
            } else {
                Bson::String(n.to_string())
            }
        }
        Value::Float32(f) => Bson::Double(f64::from(*f)),
        Value::Float64(f) => Bson::Double(*f),
        Value::BigInt(big) => Bson::String(big.to_string()),
        Value::Decimal(big) => match Decimal128::from_str(&big.to_string()) {
            Ok(d) => Bson::Decimal128(d),
            Err(_) => Bson::String(big.to_string()),
        },
        Value::Text(s) => Bson::String(s.clone()),
        Value::Bytes(b) => Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: b.clone(),
        }),
        Value::Date(d) => {
            let dt = d
                .and_hms_opt(0, 0, 0)
                .ok_or_else(|| RuntimeError::Other("invalid date".into()))?;
            let utc = Utc.from_utc_datetime(&dt);
            Bson::DateTime(bson::DateTime::from_millis(utc.timestamp_millis()))
        }
        Value::Timestamp(ts) => Bson::DateTime(bson::DateTime::from_millis(ts.timestamp_millis())),
        Value::Uuid(u) => Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Uuid,
            bytes: u.as_bytes().to_vec(),
        }),
        Value::Json(j) => bson::to_bson(j).map_err(RuntimeError::backend)?,
    })
}

/// Best-effort `DataType` inferred from a single BSON value.
/// `Value::Null` returns `None` because nullability is decided at
/// the field level after merging samples.
pub fn infer_type(b: &Bson) -> Option<DataType> {
    Some(match b {
        Bson::Null => return None,
        Bson::Boolean(_) => DataType::Bool,
        Bson::Int32(_) => DataType::Int32,
        Bson::Int64(_) => DataType::Int64,
        Bson::Double(_) => DataType::Float64,
        Bson::String(_) => DataType::Text { size: None },
        Bson::Binary(b) => match b.subtype {
            bson::spec::BinarySubtype::Uuid | bson::spec::BinarySubtype::UuidOld => DataType::Uuid,
            _ => DataType::Bytes { size: None },
        },
        Bson::ObjectId(_) => DataType::Bytes { size: Some(12) },
        Bson::DateTime(_) => DataType::Timestamp,
        Bson::Decimal128(_) => DataType::Decimal {
            precision: None,
            scale: None,
        },
        Bson::Document(_) | Bson::Array(_) => DataType::Json,
        _ => return None,
    })
}

fn decimal_to_string(d: &Decimal128) -> String {
    d.to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bson::oid::ObjectId;

    #[test]
    fn null_round_trip() {
        let v = from_bson(&Bson::Null).unwrap();
        assert!(matches!(v, Value::Null));
        assert!(matches!(to_bson(&Value::Null).unwrap(), Bson::Null));
    }

    #[test]
    fn int32_round_trip() {
        let v = from_bson(&Bson::Int32(42)).unwrap();
        assert_eq!(v, Value::Int32(42));
        assert!(matches!(to_bson(&v).unwrap(), Bson::Int32(42)));
    }

    #[test]
    fn objectid_to_bytes() {
        let oid = ObjectId::new();
        let v = from_bson(&Bson::ObjectId(oid)).unwrap();
        match v {
            Value::Bytes(b) => assert_eq!(b.len(), 12),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    #[test]
    fn infer_type_objectid_is_bytes12() {
        assert_eq!(
            infer_type(&Bson::ObjectId(ObjectId::new())),
            Some(DataType::Bytes { size: Some(12) })
        );
    }

    #[test]
    fn infer_type_string() {
        assert_eq!(
            infer_type(&Bson::String("x".into())),
            Some(DataType::Text { size: None })
        );
    }

    #[test]
    fn infer_type_null_is_none() {
        assert_eq!(infer_type(&Bson::Null), None);
    }

    #[test]
    fn uuid_round_trip_via_binary_subtype() {
        let u = uuid::Uuid::new_v4();
        let encoded = to_bson(&Value::Uuid(u)).unwrap();
        match &encoded {
            Bson::Binary(b) => {
                assert!(matches!(
                    b.subtype,
                    bson::spec::BinarySubtype::Uuid | bson::spec::BinarySubtype::UuidOld
                ));
                assert_eq!(b.bytes.len(), 16);
            }
            other => panic!("expected uuid binary, got {other:?}"),
        }
        // The codec exposes uuid binaries as `DataType::Uuid` for
        // schema inference, but the value comes back as `Value::Bytes`
        // because the canonical `from_bson` does not re-discriminate
        // by subtype. This is a documented MVP limitation.
        let inferred = infer_type(&encoded);
        assert_eq!(inferred, Some(air_elt_core::types::DataType::Uuid));
    }

    #[test]
    fn timestamp_truncates_to_milliseconds() {
        use chrono::{TimeZone, Utc};
        let ts = Utc.timestamp_opt(1_700_000_000, 123_456_789).unwrap();
        let encoded = to_bson(&Value::Timestamp(ts)).unwrap();
        let decoded = from_bson(&encoded).unwrap();
        match decoded {
            Value::Timestamp(t) => {
                // Sub-ms part is floored to whole milliseconds.
                assert_eq!(t.timestamp_millis(), 1_700_000_000_123);
                assert_eq!(t.timestamp_subsec_nanos() % 1_000_000, 0);
            }
            other => panic!("expected Value::Timestamp, got {other:?}"),
        }
    }

    #[test]
    fn nested_document_becomes_json_value() {
        use bson::doc as bdoc;
        let nested = Bson::Document(bdoc! { "city": "Berlin", "zip": "10115" });
        let v = from_bson(&nested).unwrap();
        match v {
            Value::Json(j) => {
                assert_eq!(j["city"].as_str(), Some("Berlin"));
                assert_eq!(j["zip"].as_str(), Some("10115"));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }
}
