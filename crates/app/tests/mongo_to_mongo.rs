//! Same-vendor: MongoDB source → MongoDB sink, MongoDB storage,
//! exercising the wildcard raw-passthrough path
//! (`mapping = ["*"]` with both sides schemaless).
//!
//! Proves that the BSON-fidelity contract holds end-to-end. The runner
//! must carry the source document as a
//! `Value::Custom(BsonObjectValue(...))` row through the pipeline and
//! re-emit it on the sink without ever flattening it through JSON.
//! A JSON detour would silently corrupt:
//!
//!   * `Decimal128` — has no JSON-native representation; would
//!     downgrade to a string or extended-JSON sub-document and lose
//!     the native BSON type tag.
//!   * `ObjectId` — would land as a 24-hex string instead of a true
//!     `Bson::ObjectId`.
//!   * `DateTime` — would land as an RFC3339 string instead of a true
//!     `Bson::DateTime`.
//!
//! The test asserts the strongest possible signal: serialise both the
//! source and sink documents via `bson::to_vec` and compare bytes.
//! Byte-equal means every BSON type tag, every length prefix and every
//! field-order decision was preserved — i.e. the document never left
//! BSON form. The Decimal128 / ObjectId / DateTime / nested-array
//! fixtures are picked specifically because each of them survives only
//! on the BSON path.

#![allow(clippy::unwrap_used)]

use std::str::FromStr;

use air_elt_app::App;
use air_elt_commons_testing::mongo::mongo_pool;
use bson::{Bson, Decimal128, doc, oid::ObjectId, spec::BinarySubtype};
use chrono::{TimeZone, Utc};
use futures::TryStreamExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_mongo_wildcard_raw_passthrough_preserves_bson_fidelity() {
    let mongo = mongo_pool().await;

    let src_db = format!("{}_src", mongo.database);
    let dst_db = format!("{}_dst", mongo.database);
    let state_db = format!("{}_state", mongo.database);

    // Two documents covering every BSON variant the runner must
    // forward verbatim. If the pipeline ever detoured through JSON or
    // through the per-field codec, every one of these would visibly
    // degrade.
    let oid_a = ObjectId::from_bytes([
        0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
    ]);
    let oid_b = ObjectId::from_bytes([
        0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x02,
    ]);
    let dec_a = Decimal128::from_str("1.23E+45").unwrap();
    // A different exponent on the second doc protects against a fixture
    // that accidentally serialises both to the same bytes.
    let dec_b = Decimal128::from_str("9.87E-10").unwrap();
    let ts_a = bson::DateTime::from_millis(
        Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0)
            .unwrap()
            .timestamp_millis(),
    );
    let ts_b = bson::DateTime::from_millis(
        Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 1)
            .unwrap()
            .timestamp_millis(),
    );
    let uid_a = uuid::Uuid::parse_str("a3d4b1f0-e1c2-4d2a-9b8e-1122334455aa").unwrap();
    let uid_b = uuid::Uuid::parse_str("b1c2d3e4-f5a6-4b7c-8d9e-aabbccddeeff").unwrap();

    let doc_a = doc! {
        "_id": oid_a,
        // Bool / Int32 / Int64 / Double — every numeric BSON kind.
        "flag": true,
        "i32": 42_i32,
        "i64": 9_000_000_000_i64,
        "f64": std::f64::consts::PI,
        // String, plus Decimal128 / DateTime guarded explicitly below.
        "name": "alice",
        "amount": dec_a,
        "created_at": ts_a,
        // Binary generic + Binary UUID subtype. Subtype tags only
        // survive on the raw-passthrough path.
        "blob": Bson::Binary(bson::Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![0xDE, 0xAD, 0xBE, 0xEFu8],
        }),
        "uid": Bson::Binary(bson::Binary {
            subtype: BinarySubtype::Uuid,
            bytes: uid_a.as_bytes().to_vec(),
        }),
        // BSON null is its own variant; arrays preserve element types.
        "missing": Bson::Null,
        "tags": ["a", "b", "c"],
        "nested": { "x": 1_i32, "y": "z", "deep": { "k": 2_i32 } },
        // RegularExpression, MinKey and MaxKey are not representable
        // by the per-field codec (`from_bson` errors on them) — only
        // the raw-passthrough path forwards them intact.
        "pattern": Bson::RegularExpression(bson::Regex {
            pattern: "^foo".into(),
            options: "i".into(),
        }),
        "min": Bson::MinKey,
        "max": Bson::MaxKey,
    };
    let doc_b = doc! {
        "_id": oid_b,
        "flag": false,
        "i32": -7_i32,
        "i64": -9_000_000_000_i64,
        "f64": std::f64::consts::E,
        "name": "bob",
        "amount": dec_b,
        "created_at": ts_b,
        "blob": Bson::Binary(bson::Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![0x00, 0x01, 0x02, 0x03],
        }),
        "uid": Bson::Binary(bson::Binary {
            subtype: BinarySubtype::Uuid,
            bytes: uid_b.as_bytes().to_vec(),
        }),
        "missing": Bson::Null,
        "tags": ["x", "y"],
        "nested": { "x": 2_i32, "y": "w", "deep": { "k": 3_i32 } },
        "pattern": Bson::RegularExpression(bson::Regex {
            pattern: "bar$".into(),
            options: "".into(),
        }),
        "min": Bson::MinKey,
        "max": Bson::MaxKey,
    };

    let src_coll = mongo
        .client
        .database(&src_db)
        .collection::<bson::Document>("items");
    src_coll
        .insert_many([doc_a.clone(), doc_b.clone()])
        .await
        .unwrap();

    let mongo_url = mongo.url.clone();

    // Raw passthrough requires:
    //   * `mapping = ["*"]`,
    //   * source AND sink schemaless (Mongo on both sides),
    //   * no `cursor.fields` (raw flows reject column cursors),
    //   * no `conflict.key` (same).
    // The cursor block itself is mandatory in the config schema, so we
    // declare it with an empty `fields` list and a short interval.
    let config_toml = format!(
        r#"
[[sources]]
name = "mongo_src"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{src_db}" }}

[[sinks]]
name = "mongo_sink"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{dst_db}" }}

[[storages]]
name = "mongo_state"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{state_db}" }}

[flow.items]
source = "mongo_src"
sink = "mongo_sink"
storage = "mongo_state"
from = "items"
to = "items"
batch-limit = 16
cursor = {{ fields = [], order = "asc", interval = "100ms" }}

[flow.items.mapping]
"*" = "*"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let sink_coll = mongo
        .client
        .database(&dst_db)
        .collection::<bson::Document>("items");
    let mut cursor = sink_coll
        .find(doc! {})
        .sort(doc! { "_id": 1 })
        .await
        .unwrap();
    let mut got = Vec::new();
    while let Some(d) = cursor.try_next().await.unwrap() {
        got.push(d);
    }
    assert_eq!(got.len(), 2, "both source documents land in the sink");

    // The collection's own `_id` ordering matches insertion order of
    // our two ObjectIds (lexicographic on the byte arrays).
    let pairs = [(&doc_a, &got[0]), (&doc_b, &got[1])];
    for (src, sink) in pairs {
        // 1. Type-tag spot-checks. If the runner had flattened to JSON,
        // these would arrive as String / Document instead of their
        // native BSON variants.
        let amount = sink.get("amount").expect("amount present");
        assert!(
            matches!(amount, Bson::Decimal128(_)),
            "amount must remain BSON Decimal128; got {amount:?}"
        );
        let id = sink.get("_id").expect("_id present");
        assert!(
            matches!(id, Bson::ObjectId(_)),
            "_id must remain BSON ObjectId; got {id:?}"
        );
        let created = sink.get("created_at").expect("created_at present");
        assert!(
            matches!(created, Bson::DateTime(_)),
            "created_at must remain BSON DateTime; got {created:?}"
        );
        let flag = sink.get("flag").expect("flag present");
        assert!(
            matches!(flag, Bson::Boolean(_)),
            "flag must remain BSON Boolean; got {flag:?}"
        );
        let i32_value = sink.get("i32").expect("i32 present");
        assert!(
            matches!(i32_value, Bson::Int32(_)),
            "i32 must remain BSON Int32 (not widened to Int64); got {i32_value:?}"
        );
        let i64_value = sink.get("i64").expect("i64 present");
        assert!(
            matches!(i64_value, Bson::Int64(_)),
            "i64 must remain BSON Int64; got {i64_value:?}"
        );
        let f64_value = sink.get("f64").expect("f64 present");
        assert!(
            matches!(f64_value, Bson::Double(_)),
            "f64 must remain BSON Double; got {f64_value:?}"
        );
        let name = sink.get("name").expect("name present");
        assert!(
            matches!(name, Bson::String(_)),
            "name must remain BSON String; got {name:?}"
        );
        let blob = sink.get("blob").expect("blob present");
        match blob {
            Bson::Binary(b) => assert!(
                matches!(b.subtype, BinarySubtype::Generic),
                "blob must remain BSON Binary subtype Generic; got {:?}",
                b.subtype
            ),
            other => panic!("blob must remain BSON Binary; got {other:?}"),
        }
        let uid = sink.get("uid").expect("uid present");
        match uid {
            Bson::Binary(b) => assert!(
                matches!(b.subtype, BinarySubtype::Uuid | BinarySubtype::UuidOld),
                "uid must keep its UUID subtype (per-field codec would \
                 normalise it to Generic); got {:?}",
                b.subtype
            ),
            other => panic!("uid must remain BSON Binary; got {other:?}"),
        }
        let tags = sink.get("tags").expect("tags present");
        assert!(
            matches!(tags, Bson::Array(_)),
            "tags must remain a BSON array; got {tags:?}"
        );
        let nested = sink.get("nested").expect("nested present");
        assert!(
            matches!(nested, Bson::Document(_)),
            "nested must remain a BSON sub-document; got {nested:?}"
        );
        let missing = sink.get("missing").expect("missing present");
        assert!(
            matches!(missing, Bson::Null),
            "BSON Null must round-trip as Null (and not be dropped); got {missing:?}"
        );
        let pattern = sink.get("pattern").expect("pattern present");
        assert!(
            matches!(pattern, Bson::RegularExpression(_)),
            "RegularExpression must survive raw-passthrough (the per-field \
             codec rejects it outright); got {pattern:?}"
        );
        let min = sink.get("min").expect("min present");
        assert!(
            matches!(min, Bson::MinKey),
            "MinKey must survive raw-passthrough; got {min:?}"
        );
        let max = sink.get("max").expect("max present");
        assert!(
            matches!(max, Bson::MaxKey),
            "MaxKey must survive raw-passthrough; got {max:?}"
        );

        // 2. Strongest signal: byte-equal serialisation. Decimal128
        // canonical encoding only round-trips byte-for-byte if the
        // value never left BSON form, so this assertion subsumes the
        // type-tag checks above.
        let src_bytes = bson::to_vec(src).expect("encode source doc");
        let sink_bytes = bson::to_vec(sink).expect("encode sink doc");
        assert_eq!(
            src_bytes, sink_bytes,
            "raw passthrough must preserve the BSON encoding byte-for-byte"
        );
    }
}
