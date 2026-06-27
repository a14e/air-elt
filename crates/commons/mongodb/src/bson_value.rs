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
//! - **Document → `Value::Object`**: BSON documents are recursively
//!   converted into `Value::Object(Vec<(String, Value)>)`, preserving
//!   field order and enabling lossless round-trips through the
//!   canonical model without the `serde_json::Value` intermediate.
//! - **Array → `Value::Json`**: BSON arrays are still converted via
//!   `bson::from_bson` into a `serde_json::Value`. Nested ObjectIds,
//!   Decimal128, Binary and DateTime values inside arrays are
//!   serialised as the driver's extended-JSON shapes. If a pipeline
//!   needs full fidelity for nested BSON it should map the leaf paths
//!   explicitly rather than pulling the whole subdocument as JSON.

use bigdecimal::BigDecimal;
use bson::{Bson, Decimal128};
use chrono::{SecondsFormat, TimeZone, Utc};
use serde_json::Value as Json;
use std::str::FromStr;

use air_elt_core::error::{JsonEncodeError, RuntimeError, RuntimeResult};
use air_elt_core::types::{DataType, Value};

use crate::types::{MongoJsType, MongoJsValue, MongoObjectIdType, MongoObjectIdValue};

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
        Bson::ObjectId(oid) => Value::Custom(Box::new(MongoObjectIdValue(oid.bytes()))),
        Bson::JavaScriptCode(s) => Value::Custom(Box::new(MongoJsValue(s.clone()))),
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
        Bson::Document(doc) => {
            let entries: Vec<(String, Value)> = doc
                .iter()
                .map(|(k, v)| Ok((k.clone(), from_bson(v)?)))
                .collect::<RuntimeResult<_>>()?;
            Value::Object(entries)
        }
        Bson::Array(_) => {
            let json: Json = bson::from_bson(b.clone()).map_err(RuntimeError::backend)?;
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
/// Owning counterpart of [`to_bson`]. Moves the inner payload out
/// where the canonical `Value` can give it up — notably `Value::Json`
/// (whose `serde_json::Value` is bson-encoded byte-for-byte without an
/// extra clone), `Value::Object` (recursively converted to a BSON
/// `Document`), and `Value::Custom(BsonObjectValue)` (whose `Document`
/// is moved out of the box). Other variants forward to [`to_bson`]
/// against a borrowed view because they're already cheap to copy.
pub fn to_bson_owned(v: Value) -> RuntimeResult<Bson> {
    match v {
        Value::Json(j) => bson::to_bson(&j).map_err(RuntimeError::backend),
        Value::Object(entries) => {
            let doc: bson::Document = entries
                .into_iter()
                .map(|(k, v)| Ok((k, to_bson_owned(v)?)))
                .collect::<RuntimeResult<_>>()?;
            Ok(Bson::Document(doc))
        }
        Value::Custom(inner) => {
            let dt = inner.dyn_type();
            let kind = dt.kind();
            // Try the owned downcasts first — these move the payload
            // out of the box. Fall back to the borrowed encoder for
            // the remaining custom kinds.
            let any: Box<dyn std::any::Any> = inner.into_any();
            let any = match any.downcast::<crate::types::BsonObjectValue>() {
                Ok(bo) => return Ok(Bson::Document(bo.0)),
                Err(b) => b,
            };
            let any = match any.downcast::<MongoObjectIdValue>() {
                Ok(oid) => return Ok(Bson::ObjectId(bson::oid::ObjectId::from_bytes(oid.0))),
                Err(b) => b,
            };
            match any.downcast::<MongoJsValue>() {
                Ok(js) => Ok(Bson::JavaScriptCode(js.0)),
                Err(_) => Err(RuntimeError::Other(format!(
                    "unsupported Value::Custom kind for mongo encoder: {kind:?}"
                ))),
            }
        }
        Value::Text(s) => Ok(Bson::String(s)),
        Value::Bytes(b) => Ok(Bson::Binary(bson::Binary {
            subtype: bson::spec::BinarySubtype::Generic,
            bytes: b,
        })),
        // Remaining variants are Copy or trivially small — defer.
        other => to_bson(&other),
    }
}

pub fn to_bson(v: &Value) -> RuntimeResult<Bson> {
    Ok(match v {
        Value::Null => Bson::Null,
        Value::Bool(b) => Bson::Boolean(*b),
        Value::Int8(n) => Bson::Int32(i32::from(*n)),
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
        // BSON has no IP type; encode as canonical text string.
        // Source-side stays as Value::Text — users opt into typed
        // semantics via a Text → Ipv4/Ipv6 convert in mapping.
        Value::Ipv4(a) => Bson::String(a.to_string()),
        Value::Ipv6(a) => Bson::String(a.to_string()),
        Value::Json(j) => bson::to_bson(j).map_err(RuntimeError::backend)?,
        Value::Object(entries) => {
            let mut doc = bson::Document::new();
            for (key, val) in entries {
                doc.insert(key.clone(), to_bson(val)?);
            }
            Bson::Document(doc)
        }
        // MongoDB natively supports arrays. Each canonical element is
        // recursively encoded through the same Value -> Bson codec so
        // nested arrays/objects/customs round-trip faithfully.
        Value::Array(items) => {
            let encoded: Vec<Bson> = items.iter().map(to_bson).collect::<RuntimeResult<_>>()?;
            Bson::Array(encoded)
        }
        Value::Interval(_) => {
            return Err(RuntimeError::Other(
                "Value::Interval (redis-only type) has no BSON encoding".to_string(),
            ));
        }
        Value::Custom(inner) => {
            let any = inner.as_any();
            if let Some(oid) = any.downcast_ref::<MongoObjectIdValue>() {
                Bson::ObjectId(bson::oid::ObjectId::from_bytes(oid.0))
            } else if let Some(js) = any.downcast_ref::<MongoJsValue>() {
                Bson::JavaScriptCode(js.0.clone())
            } else {
                return Err(RuntimeError::Other(format!(
                    "unsupported Value::Custom kind for mongo encoder: {:?}",
                    {
                        let dt = inner.dyn_type();
                        dt.kind().to_string()
                    }
                )));
            }
        }
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
        Bson::ObjectId(_) => DataType::Custom(Box::new(MongoObjectIdType)),
        Bson::JavaScriptCode(_) => DataType::Custom(Box::new(MongoJsType)),
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

/// Encode a `Bson` value as a `serde_json::Value` for the JSON
/// auto-pack path (`*:body` mapping) and the `BsonObject` custom
/// type's `to_json` impl.
///
/// Encoding is Debezium-compatible without prefixes:
/// - `ObjectId` → 24-char lowercase hex string.
/// - `Decimal128` → string (canonical decimal representation).
/// - `DateTime` → RFC3339 UTC string.
/// - `Binary` → bare hex string (lowercase).
/// - `Document` / `Array` → recursive JSON object / array.
/// - `Null` → `Json::Null`.
/// - Numeric: Int32/Int64 as JSON numbers; Double NaN/±Inf → `null`.
///
/// Returns `JsonEncodeError::Variant` for BSON variants that have no
/// JSON encoding rule (regex, JS code, MinKey/MaxKey, etc.).
///
/// Recursion through nested `Document` / `Array` is depth-tracked and
/// capped at [`air_elt_core::types::json_encode::MAX_JSON_DEPTH`] (100),
/// matching the canonical encoder. Pathological nesting returns
/// [`JsonEncodeError::DepthExceeded`].
pub fn bson_to_json(b: &Bson) -> Result<Json, JsonEncodeError> {
    bson_to_json_at_depth(b, 0)
}

fn bson_to_json_at_depth(b: &Bson, depth: usize) -> Result<Json, JsonEncodeError> {
    if depth > air_elt_core::types::json_encode::MAX_JSON_DEPTH {
        return Err(JsonEncodeError::DepthExceeded);
    }
    Ok(match b {
        Bson::Null | Bson::Undefined => Json::Null,
        Bson::Boolean(v) => Json::Bool(*v),
        Bson::Int32(v) => Json::Number((*v).into()),
        Bson::Int64(v) => Json::Number((*v).into()),
        Bson::Double(v) => {
            if v.is_finite() {
                serde_json::Number::from_f64(*v)
                    .map(Json::Number)
                    .unwrap_or(Json::Null)
            } else {
                // NaN / ±Inf → null.
                Json::Null
            }
        }
        Bson::String(s) => Json::String(s.clone()),
        Bson::ObjectId(oid) => Json::String(oid.to_hex()),
        Bson::Decimal128(d) => Json::String(decimal_to_string(d)),
        Bson::DateTime(dt) => {
            let millis = dt.timestamp_millis();
            let secs = millis.div_euclid(1000);
            let nanos = (millis.rem_euclid(1000) as u32) * 1_000_000;
            let ts = Utc.timestamp_opt(secs, nanos).single().ok_or_else(|| {
                JsonEncodeError::Variant(format!("invalid BSON DateTime millis={millis}"))
            })?;
            Json::String(ts.to_rfc3339_opts(SecondsFormat::Millis, true))
        }
        Bson::Binary(bin) => {
            // Bare hex (lowercase). UUID subtype takes precedence
            // and emits the canonical UUID string when the payload is
            // 16 bytes.
            if matches!(
                bin.subtype,
                bson::spec::BinarySubtype::Uuid | bson::spec::BinarySubtype::UuidOld
            ) && bin.bytes.len() == 16
            {
                let mut arr = [0_u8; 16];
                arr.copy_from_slice(&bin.bytes);
                Json::String(uuid::Uuid::from_bytes(arr).to_string())
            } else {
                Json::String(hex::encode(&bin.bytes))
            }
        }
        Bson::JavaScriptCode(s) => Json::String(s.clone()),
        Bson::Symbol(s) => Json::String(s.clone()),
        Bson::Document(d) => {
            let mut map = serde_json::Map::with_capacity(d.len());
            for (k, v) in d {
                map.insert(k.clone(), bson_to_json_at_depth(v, depth + 1)?);
            }
            Json::Object(map)
        }
        Bson::Array(a) => {
            let mut out = Vec::with_capacity(a.len());
            for v in a {
                out.push(bson_to_json_at_depth(v, depth + 1)?);
            }
            Json::Array(out)
        }
        Bson::Timestamp(ts) => {
            // BSON Timestamp is a replication-internal pair (T, I);
            // encode as `{"t": secs, "i": inc}` to preserve the shape.
            let mut map = serde_json::Map::with_capacity(2);
            map.insert("t".into(), Json::Number(ts.time.into()));
            map.insert("i".into(), Json::Number(ts.increment.into()));
            Json::Object(map)
        }
        other => {
            return Err(JsonEncodeError::Variant(format!(
                "bson_to_json: unsupported variant {:?}",
                other.element_type()
            )));
        }
    })
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
    fn objectid_decodes_to_custom_value() {
        let oid = ObjectId::new();
        let v = from_bson(&Bson::ObjectId(oid)).unwrap();
        match &v {
            Value::Custom(inner) => {
                let casted = inner
                    .as_any()
                    .downcast_ref::<MongoObjectIdValue>()
                    .expect("downcast");
                assert_eq!(casted.0, oid.bytes());
            }
            other => panic!("expected Value::Custom, got {other:?}"),
        }
    }

    #[test]
    fn objectid_round_trips_back_to_bson_object_id() {
        let oid = ObjectId::new();
        let v = from_bson(&Bson::ObjectId(oid)).unwrap();
        let encoded = to_bson(&v).unwrap();
        match encoded {
            Bson::ObjectId(round) => assert_eq!(round, oid),
            other => panic!("expected Bson::ObjectId, got {other:?}"),
        }
    }

    #[test]
    fn javascript_code_decodes_to_custom_value() {
        let code = "function () { return 1; }".to_string();
        let v = from_bson(&Bson::JavaScriptCode(code.clone())).unwrap();
        match &v {
            Value::Custom(inner) => {
                let casted = inner
                    .as_any()
                    .downcast_ref::<MongoJsValue>()
                    .expect("downcast");
                assert_eq!(casted.0, code);
            }
            other => panic!("expected Value::Custom, got {other:?}"),
        }
    }

    #[test]
    fn javascript_code_round_trips_back_to_bson() {
        let code = "function f() { return 42; }".to_string();
        let v = from_bson(&Bson::JavaScriptCode(code.clone())).unwrap();
        let encoded = to_bson(&v).unwrap();
        match encoded {
            Bson::JavaScriptCode(s) => assert_eq!(s, code),
            other => panic!("expected Bson::JavaScriptCode, got {other:?}"),
        }
    }

    #[test]
    fn infer_type_objectid_is_custom() {
        let dt = infer_type(&Bson::ObjectId(ObjectId::new())).expect("Some");
        match dt {
            DataType::Custom(t) => assert_eq!(t.kind(), "mongodb.object_id"),
            other => panic!("expected DataType::Custom, got {other:?}"),
        }
    }

    #[test]
    fn infer_type_javascript_is_custom() {
        let dt = infer_type(&Bson::JavaScriptCode("x".into())).expect("Some");
        match dt {
            DataType::Custom(t) => assert_eq!(t.kind(), "mongodb.javascript"),
            other => panic!("expected DataType::Custom, got {other:?}"),
        }
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
    fn nested_document_becomes_object_value() {
        use bson::doc as bdoc;
        let nested = Bson::Document(bdoc! { "city": "Berlin", "zip": "10115" });
        let v = from_bson(&nested).unwrap();
        match v {
            Value::Object(entries) => {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].0, "city");
                assert_eq!(entries[0].1, Value::Text("Berlin".into()));
                assert_eq!(entries[1].0, "zip");
                assert_eq!(entries[1].1, Value::Text("10115".into()));
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn to_bson_owned_unknown_custom_kind_errors() {
        use air_elt_core::types::convert::ConvertError;
        use air_elt_core::types::convert::context::ConversionContext;
        use air_elt_core::types::dynamic::{DynType, DynValue};
        use std::any::Any;

        #[derive(Debug, Clone)]
        struct UnknownType;
        impl DynType for UnknownType {
            fn as_any(&self) -> &dyn Any {
                self
            }

            fn kind(&self) -> &str {
                "test.unknown"
            }
            fn cursor_compatible(&self) -> bool {
                false
            }
            fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
                false
            }
            fn can_construct_from(&self, _s: &DataType, _trunc: bool) -> bool {
                false
            }
            fn convert(
                &self,
                _v: Value,
                target: &DataType,
                _c: &ConversionContext,
            ) -> Result<Value, ConvertError> {
                Err(ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(UnknownType)),
                    dst: target.clone(),
                })
            }
            fn construct(
                &self,
                _v: Value,
                src: &DataType,
                _c: &ConversionContext,
            ) -> Result<Value, ConvertError> {
                Err(ConvertError::Unsupported {
                    src: src.clone(),
                    dst: DataType::Custom(Box::new(UnknownType)),
                })
            }
            fn clone_box(&self) -> Box<dyn DynType> {
                Box::new(UnknownType)
            }
        }

        #[derive(Debug, Clone)]
        struct UnknownValue;
        impl DynValue for UnknownValue {
            fn dyn_type(&self) -> Box<dyn DynType> {
                Box::new(UnknownType)
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn into_any(self: Box<Self>) -> Box<dyn Any> {
                self
            }
            fn is_equal(&self, _o: &dyn DynValue) -> bool {
                false
            }
            fn clone_box(&self) -> Box<dyn DynValue> {
                Box::new(UnknownValue)
            }
        }

        let err = to_bson_owned(Value::Custom(Box::new(UnknownValue)))
            .expect_err("unknown kind must error");
        match err {
            RuntimeError::Other(msg) => assert!(
                msg.contains("test.unknown"),
                "error must mention kind, got: {msg}"
            ),
            other => panic!("expected RuntimeError::Other, got {other:?}"),
        }
    }

    #[test]
    fn document_round_trips_through_object() {
        use bson::doc as bdoc;
        let doc = bdoc! { "name": "Alice", "age": 30_i32 };
        let v = from_bson(&Bson::Document(doc.clone())).unwrap();
        assert!(matches!(v, Value::Object(_)));
        let back = to_bson_owned(v).unwrap();
        assert_eq!(back, Bson::Document(doc));
    }

    #[test]
    fn nested_document_round_trips_through_object() {
        use bson::doc as bdoc;
        let doc = bdoc! {
            "user": { "name": "Bob", "score": 42_i32 },
            "tags": ["a", "b"],
        };
        let v = from_bson(&Bson::Document(doc.clone())).unwrap();
        // Top level is Value::Object; nested document is also Object.
        match &v {
            Value::Object(entries) => {
                assert_eq!(entries.len(), 2);
                assert!(matches!(&entries[0].1, Value::Object(_)));
                assert!(matches!(&entries[1].1, Value::Json(_))); // Array stays Json
            }
            other => panic!("expected Object, got {other:?}"),
        }
        // Round-trip back to BSON preserves the structure.
        let back = to_bson_owned(v).unwrap();
        assert_eq!(back, Bson::Document(doc));
    }

    #[test]
    fn to_bson_borrowed_object() {
        let obj = Value::Object(vec![
            ("x".into(), Value::Int32(1)),
            ("y".into(), Value::Text("hello".into())),
        ]);
        let bson = to_bson(&obj).unwrap();
        match bson {
            Bson::Document(d) => {
                assert_eq!(d.get_i32("x").unwrap(), 1);
                assert_eq!(d.get_str("y").unwrap(), "hello");
            }
            other => panic!("expected Document, got {other:?}"),
        }
    }

    #[test]
    fn array_stays_as_json_value() {
        let arr = Bson::Array(vec![Bson::Int32(1), Bson::Int32(2)]);
        let v = from_bson(&arr).unwrap();
        assert!(
            matches!(v, Value::Json(_)),
            "Array should remain Value::Json"
        );
    }

    #[test]
    fn value_array_encodes_to_bson_array() {
        // Mongo natively supports arrays. The Value -> Bson write codec
        // must recursively encode each canonical element. `from_bson`
        // intentionally maps `Bson::Array` back to `Value::Json` (the
        // documented read-path behaviour), so this asserts the write
        // mapping directly: `Value::Array([Int64, Text])` ->
        // `Bson::Array([Int64, String])`.
        let value = Value::Array(vec![Value::Int64(7), Value::Text("hello".into())]);
        let encoded = to_bson(&value).unwrap();
        match encoded {
            Bson::Array(items) => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0], Bson::Int64(7));
                assert_eq!(items[1], Bson::String("hello".into()));
            }
            other => panic!("expected Bson::Array, got {other:?}"),
        }
    }

    #[test]
    fn value_array_owned_encodes_to_bson_array() {
        // `to_bson_owned` forwards arrays through its catch-all to the
        // borrowed encoder; assert the same recursive shape lands.
        let value = Value::Array(vec![Value::Bool(true), Value::Int32(3)]);
        let encoded = to_bson_owned(value).unwrap();
        assert_eq!(
            encoded,
            Bson::Array(vec![Bson::Boolean(true), Bson::Int32(3)])
        );
    }

    #[test]
    fn nested_value_array_encodes_recursively() {
        // Nested arrays exercise the recursive arm.
        let value = Value::Array(vec![
            Value::Array(vec![Value::Int32(1)]),
            Value::Text("x".into()),
        ]);
        let encoded = to_bson(&value).unwrap();
        assert_eq!(
            encoded,
            Bson::Array(vec![
                Bson::Array(vec![Bson::Int32(1)]),
                Bson::String("x".into()),
            ])
        );
    }
}
