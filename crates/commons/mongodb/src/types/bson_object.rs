//! `mongodb.bson_object` custom type — raw whole-document passthrough.
//!
//! Used by wildcard mapping (`mapping = ["*"]`) when both the
//! source and the sink are schemaless (i.e. Mongo→Mongo). Instead of
//! enumerating columns, the source emits one row per document with a
//! single `Value::Custom(BsonObjectValue(doc))` carrying the entire
//! document; the sink recognises the same shape and writes the
//! document back via `insertMany([doc])`, skipping the per-field
//! path-set machinery.
//!
//! ## Conversion matrix
//!
//! Outbound (`BsonObjectType -> canonical`):
//! - `Custom("mongodb.bson_object")` (identity, handled by the
//!   dispatcher's both-sides-Custom arm).
//! - `Json` — encoded via [`crate::bson_value::bson_to_json`] using
//!   Debezium-compatible wire-format rules (without prefixes).
//!
//! Inbound (`canonical -> BsonObjectType`): not supported. The custom
//! is produced only by the source's raw-mode path — there's no
//! `Json -> BsonObject` arm because once a document is JSON-flattened
//! the BSON variants are gone.
//!
//! ## Cursor
//!
//! `can_be_cursor() = false`. Whole documents have no useful order.

use std::any::Any;

use bson::Document;

use air_elt_core::error::JsonEncodeError;
use air_elt_core::types::convert::ConvertError;
use air_elt_core::types::convert::context::ConversionContext;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::dynamic::{DynType, DynValue};
use air_elt_core::types::value::Value;

use crate::bson_value;

/// Schema-side descriptor for `mongodb.bson_object`.
#[derive(Debug, Clone, Copy)]
pub struct BsonObjectType;

/// Runtime carrier for a whole BSON document.
#[derive(Debug, Clone, PartialEq)]
pub struct BsonObjectValue(pub Document);

impl BsonObjectType {
    /// Single source of truth for the kind string. Sites that need to
    /// recognise a `BsonObject` `DataType::Custom(t)` should compare
    /// against this constant rather than re-spelling the literal.
    pub const KIND: &'static str = "mongodb.bson_object";
}

impl DynType for BsonObjectType {
    fn kind(&self) -> &'static str {
        Self::KIND
    }

    fn can_be_cursor(&self) -> bool {
        false
    }

    fn is_object(&self) -> bool {
        true
    }

    fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
        // Identity (Custom→Custom) is handled by the dispatcher arm
        // outside this method; from here we only need to advertise
        // Json as a permitted outbound target.
        matches!(target, DataType::Json)
    }

    fn can_construct_from(&self, _src: &DataType, _truncate: bool) -> bool {
        // Inbound construction is not supported — BsonObject is
        // produced only by the source's raw-mode path.
        false
    }

    fn convert(
        &self,
        value: Value,
        target: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        let inner = unwrap_bson_object(&value)?;
        match target {
            DataType::Json => {
                let json = bson_value::bson_to_json(&bson::Bson::Document(inner.0.clone()))
                    .map_err(|_e| ConvertError::Unsupported {
                        src: DataType::Custom(Box::new(BsonObjectType)),
                        dst: DataType::Json,
                    })?;
                Ok(Value::Json(json))
            }
            other => Err(ConvertError::Unsupported {
                src: DataType::Custom(Box::new(BsonObjectType)),
                dst: other.clone(),
            }),
        }
    }

    fn construct(
        &self,
        _value: Value,
        src: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        Err(ConvertError::Unsupported {
            src: src.clone(),
            dst: DataType::Custom(Box::new(BsonObjectType)),
        })
    }

    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(*self)
    }
}

impl DynValue for BsonObjectValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(BsonObjectType)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn std::any::Any> {
        self
    }

    fn eq_dyn(&self, other: &dyn DynValue) -> bool {
        other
            .as_any()
            .downcast_ref::<BsonObjectValue>()
            .map(|o| o.0 == self.0)
            .unwrap_or(false)
    }

    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(self.clone())
    }

    fn to_json(&self) -> Result<serde_json::Value, JsonEncodeError> {
        bson_value::bson_to_json(&bson::Bson::Document(self.0.clone()))
    }
}

fn unwrap_bson_object(v: &Value) -> Result<&BsonObjectValue, ConvertError> {
    match v {
        Value::Custom(inner) => inner
            .as_any()
            .downcast_ref::<BsonObjectValue>()
            .ok_or_else(|| ConvertError::ValueShapeMismatch {
                src: DataType::Custom(Box::new(BsonObjectType)),
            }),
        _ => Err(ConvertError::ValueShapeMismatch {
            src: DataType::Custom(Box::new(BsonObjectType)),
        }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_core::types::convert::convert;
    use bson::doc;

    fn ctx() -> ConversionContext {
        ConversionContext::passthrough()
    }

    #[test]
    fn kind_is_stable() {
        assert_eq!(BsonObjectType.kind(), "mongodb.bson_object");
        assert_eq!(BsonObjectType::KIND, "mongodb.bson_object");
        // Round-trip: a constructed instance reports the same KIND.
        let t: Box<dyn DynType> = Box::new(BsonObjectType);
        assert_eq!(t.kind(), BsonObjectType::KIND);
    }

    #[test]
    fn cannot_be_cursor() {
        assert!(!BsonObjectType.can_be_cursor());
    }

    #[test]
    fn matrix_can_convert_to_json_only() {
        let t = BsonObjectType;
        assert!(t.can_convert_to(&DataType::Json, false));
        assert!(!t.can_convert_to(&DataType::Text { size: None }, false));
        assert!(!t.can_convert_to(&DataType::Bytes { size: None }, false));
    }

    #[test]
    fn matrix_can_construct_from_nothing() {
        let t = BsonObjectType;
        assert!(!t.can_construct_from(&DataType::Json, false));
        assert!(!t.can_construct_from(&DataType::Text { size: None }, false));
    }

    #[test]
    fn to_json_simple_doc() {
        let v = BsonObjectValue(doc! { "a": 1_i32, "b": "x" });
        let j = v.to_json().unwrap();
        assert_eq!(j, serde_json::json!({ "a": 1, "b": "x" }));
    }

    #[test]
    fn to_json_decimal128_becomes_string() {
        use bson::Decimal128;
        use std::str::FromStr;
        let d = Decimal128::from_str("1.23").unwrap();
        let v = BsonObjectValue(doc! { "d": d });
        let j = v.to_json().unwrap();
        let s = j.get("d").unwrap().as_str().expect("string");
        // BSON Decimal128 stringifies to a canonical decimal form;
        // assert it parses back as the same number.
        let parsed: f64 = s.parse().unwrap();
        assert!((parsed - 1.23).abs() < 1e-9);
    }

    #[test]
    fn to_json_object_id_becomes_24_hex() {
        use bson::oid::ObjectId;
        let oid = ObjectId::from_bytes([
            0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
        ]);
        let v = BsonObjectValue(doc! { "_id": oid });
        let j = v.to_json().unwrap();
        assert_eq!(
            j.get("_id").unwrap().as_str(),
            Some("654f10800102030405000001")
        );
    }

    #[test]
    fn to_json_datetime_is_rfc3339() {
        use bson::DateTime as BDateTime;
        let dt = BDateTime::from_millis(1_700_000_000_123);
        let v = BsonObjectValue(doc! { "ts": dt });
        let j = v.to_json().unwrap();
        let s = j.get("ts").unwrap().as_str().expect("string");
        assert!(s.ends_with('Z'), "expected UTC suffix, got {s}");
        assert!(s.contains("2023-"), "expected RFC3339 with year, got {s}");
    }

    #[test]
    fn dyn_value_eq_dyn_compares_documents() {
        let a: Box<dyn DynValue> = Box::new(BsonObjectValue(doc! { "a": 1 }));
        let b: Box<dyn DynValue> = Box::new(BsonObjectValue(doc! { "a": 1 }));
        let c: Box<dyn DynValue> = Box::new(BsonObjectValue(doc! { "a": 2 }));
        assert!(a.eq_dyn(&*b));
        assert!(!a.eq_dyn(&*c));
    }

    #[test]
    fn dyn_value_clone_box_preserves_payload() {
        let v: Box<dyn DynValue> = Box::new(BsonObjectValue(doc! { "x": 1 }));
        let c = v.clone_box();
        assert!(v.eq_dyn(&*c));
    }

    #[test]
    fn matrix_identity_is_passthrough() {
        // `Custom -> Custom` of the same kind is identity per the
        // dispatcher's both-sides-Custom arm.
        let v = Value::Custom(Box::new(BsonObjectValue(doc! { "a": 1_i32 })));
        let dt = DataType::Custom(Box::new(BsonObjectType));
        let out = convert(v.clone(), &dt, &dt, &ctx()).unwrap();
        assert_eq!(out, v);
    }

    #[test]
    fn bson_object_to_json_rejects_pathological_depth() {
        use air_elt_core::error::JsonEncodeError;
        use bson::Bson;

        // Build N levels of Array nesting wrapped at the top by a
        // Document (the BsonObjectValue carrier). The top Document
        // sits at depth 0; each nested Array consumes one depth slot.
        fn nest(n: usize) -> Bson {
            let mut v = Bson::Array(vec![]);
            for _ in 0..n {
                v = Bson::Array(vec![v]);
            }
            v
        }

        // 99 nested arrays inside the wrapping document → max depth
        // reached = 100 (= `core::types::json_encode::MAX_JSON_DEPTH`).
        // Pass.
        let ok = BsonObjectValue(doc! { "a": nest(99) });
        assert!(ok.to_json().is_ok());

        // 101 nested arrays → exceeds cap. Fail.
        let bad = BsonObjectValue(doc! { "a": nest(101) });
        assert!(matches!(bad.to_json(), Err(JsonEncodeError::DepthExceeded)));
    }

    /// Custom-type parity. Each fixture asserts that the
    /// JSON wire bytes produced via the canonical bridge
    /// (`bson_value::bson_to_json`) and via the custom value's own
    /// `to_json()` are byte-equal — the two paths cannot diverge.
    /// One ObjectId/Decimal128 fixture additionally compares against
    /// raw `bson::to_vec` to prove the matrix is doing real work and
    /// hasn't collapsed to a hidden identity.
    #[test]
    fn custom_type_parity_with_binary() {
        // Binary subtype Generic round-trips through
        // `bson_value::bson_to_json` as a hex
        // string — a non-trivial encoding (raw BSON carries the
        // length prefix + subtype byte + raw bytes), so byte-equality
        // between the bridge and the dyn-value path is structural
        // proof that both share the same encoder rather than a
        // tautology.
        use bson::spec::BinarySubtype;
        let d = doc! {
            "blob": bson::Binary {
                subtype: BinarySubtype::Generic,
                bytes: vec![0xCA, 0xFE, 0xBA, 0xBE],
            },
        };
        let via_bridge = serde_json::to_vec(
            &bson_value::bson_to_json(&bson::Bson::Document(d.clone())).unwrap(),
        )
        .unwrap();
        let via_dyn = serde_json::to_vec(&BsonObjectValue(d.clone()).to_json().unwrap()).unwrap();
        assert_eq!(via_bridge, via_dyn);

        // The canonical JSON must contain the lowercase hex form.
        let json_text = String::from_utf8(via_bridge.clone()).unwrap();
        assert!(
            json_text.contains("cafebabe"),
            "expected hex-encoded bytes in JSON, got {json_text}"
        );

        // Raw BSON bytes carry length / subtype framing absent from
        // JSON — the matrix is doing real work, not collapsing to
        // identity.
        let raw_bson = bson::to_vec(&d).unwrap();
        assert_ne!(raw_bson, via_bridge);
    }

    #[test]
    fn custom_type_parity_with_object_id() {
        use bson::oid::ObjectId;
        let oid = ObjectId::from_bytes([
            0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
        ]);
        let d = doc! { "_id": oid, "n": 7_i32 };
        let via_bridge = serde_json::to_vec(
            &bson_value::bson_to_json(&bson::Bson::Document(d.clone())).unwrap(),
        )
        .unwrap();
        let via_dyn = serde_json::to_vec(&BsonObjectValue(d.clone()).to_json().unwrap()).unwrap();
        assert_eq!(via_bridge, via_dyn);

        // Raw BSON bytes must NOT match the canonical JSON bytes — the
        // matrix actually transforms the document (ObjectId → 24-hex
        // string, BSON header bytes stripped).
        let raw_bson = bson::to_vec(&d).unwrap();
        assert_ne!(raw_bson, via_bridge);
    }

    #[test]
    fn custom_type_parity_with_decimal128() {
        use bson::Decimal128;
        use std::str::FromStr;
        // `1.230` is non-trivial: Decimal128 preserves the exponent,
        // so the canonical string keeps the trailing zero — distinct
        // from the `1.23` that a naive lossy decimal would produce.
        // This pins the canonical-form contract end-to-end.
        let dec = Decimal128::from_str("1.230").unwrap();
        let canonical = dec.to_string();
        assert_eq!(canonical, "1.230", "Decimal128 canonical form changed");
        let d = doc! { "amount": dec, "label": "x" };
        let via_bridge = serde_json::to_vec(
            &bson_value::bson_to_json(&bson::Bson::Document(d.clone())).unwrap(),
        )
        .unwrap();
        let via_dyn = serde_json::to_vec(&BsonObjectValue(d.clone()).to_json().unwrap()).unwrap();
        assert_eq!(via_bridge, via_dyn);

        // The canonical form must surface in the JSON output verbatim
        // (as a quoted string). Catches a regression where the encoder
        // drops trailing zeros or rewrites the exponent.
        let json_text = String::from_utf8(via_bridge.clone()).unwrap();
        assert!(
            json_text.contains("\"1.230\""),
            "expected canonical Decimal128 string in JSON, got {json_text}"
        );

        // Raw BSON bytes for a Decimal128-bearing document must differ
        // from the canonical JSON encoding — Decimal128 → string, BSON
        // header / type tag bytes are not present in JSON.
        let raw_bson = bson::to_vec(&d).unwrap();
        assert_ne!(raw_bson, via_bridge);
    }

    #[test]
    fn custom_type_parity_with_nested_arrays() {
        let d = doc! {
            "tags": ["a", "b", "c"],
            "matrix": [[1_i32, 2_i32], [3_i32, 4_i32]],
            "meta": { "k": "v" },
        };
        let via_bridge = serde_json::to_vec(
            &bson_value::bson_to_json(&bson::Bson::Document(d.clone())).unwrap(),
        )
        .unwrap();
        let via_dyn = serde_json::to_vec(&BsonObjectValue(d.clone()).to_json().unwrap()).unwrap();
        assert_eq!(via_bridge, via_dyn);
    }

    #[test]
    fn matrix_to_json_calls_to_json() {
        // `Custom -> Json` runs through `BsonObjectType::convert`, which
        // delegates to `bson_value::bson_to_json`.
        let v = Value::Custom(Box::new(BsonObjectValue(doc! { "a": 1_i32, "b": "x" })));
        let dt_src = DataType::Custom(Box::new(BsonObjectType));
        let out = convert(v, &dt_src, &DataType::Json, &ctx()).unwrap();
        match out {
            Value::Json(j) => assert_eq!(j, serde_json::json!({ "a": 1, "b": "x" })),
            other => panic!("expected Value::Json, got {other:?}"),
        }
    }
}
