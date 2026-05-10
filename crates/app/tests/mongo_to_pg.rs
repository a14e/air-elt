//! Cross-vendor: MongoDB source → PostgreSQL sink, PostgreSQL storage,
//! exercising the `*:body` JSON auto-pack across the BSON-to-JSONB
//! boundary.
//!
//! Pre-Fix-1 (this commit) the mongo source's `read_batch` populated
//! `RawRow.body` with a `Value::Custom(BsonObjectValue(doc))` in every
//! body slot. Without a matching matrix conversion, the Custom value
//! reached the pg sink's `bind_value_separated` and tripped the
//! "unsupported custom value kind" arm — the panic this test guards
//! against. The fix routes the body slot through the canonical matrix
//! arm `BsonObject -> Json` so the value lands in the JSONB column
//! as `Value::Json(serde_json::Value::Object(...))` — every BSON type
//! is mapped through `bson_value::bson_to_json` (Decimal128 → string,
//! ObjectId → 24-hex string, Date → RFC3339 string, etc.).
//!
//! The single-flow test pulls 3 documents with mixed BSON shapes
//! (a Decimal128, an ObjectId and a sub-document) and asserts that the
//! body JSONB column on the pg sink contains the JSON encoding the
//! `BsonObjectType::convert` arm produces.

#![allow(clippy::unwrap_used)]

use std::str::FromStr;

use air_elt_app::App;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_commons_testing::pg::pg_pool;
use bson::{Decimal128, doc, oid::ObjectId};
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_pg_body_pack_routes_bson_object_through_matrix() {
    let mongo = mongo_pool().await;
    let pg = pg_pool().await;

    let src_db_mongo = format!("{}_src", mongo.database);
    let state_db_mongo = format!("{}_state", mongo.database);
    let dst_schema_pg = format!("{}_dst", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema_pg}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema_pg}\".docs (
                    id   BIGINT PRIMARY KEY,
                    body JSONB
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Seed three docs with BSON-only types so a JSON detour from the
    // source side would visibly degrade them — Decimal128 → canonical
    // string, ObjectId → 24-hex, sub-document round-trips as a JSON
    // object. Sorting by `_id` (the cursor) keeps the test order
    // deterministic.
    let src = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("docs");
    let oid = ObjectId::from_bytes([
        0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
    ]);
    let dec = Decimal128::from_str("1.230").unwrap();
    let docs_seed = vec![
        doc! {
            "_id": 1_i64,
            "name": "alice",
            "amount": dec,
            "tags": ["a", "b"],
        },
        doc! {
            "_id": 2_i64,
            "name": "bob",
            "oid": oid,
            "nested": { "city": "Berlin", "n": 7_i32 },
        },
        doc! {
            "_id": 3_i64,
            "name": "carol",
            "score": 42_i32,
        },
    ];
    for d in &docs_seed {
        src.insert_one(d.clone()).await.unwrap();
    }

    let mongo_url = mongo.url.clone();
    let pg_url = pg.url_with_search_path();

    // Mapping: explicit `_id -> id` direct + `*:body` packs the rest
    // (which under the mongo source's `read_batch` produces a
    // BsonObject body, then the matrix converts it to JSON for JSONB).
    let config_toml = format!(
        r#"
[[sources]]
name = "mongo_src"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{src_db_mongo}" }}

[[sinks]]
name = "pg_sink"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "mongo_state"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{state_db_mongo}" }}

[flow.docs]
source = "mongo_src"
sink = "pg_sink"
storage = "mongo_state"
from = "docs"
to = "{dst_schema_pg}.docs"
batch-limit = 8

mapping = [
    {{ from = "_id", to = "id", default = 0 }},
    "*:body",
]

cursor = {{ fields = ["_id"], order = "asc", interval = "100ms" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, serde_json::Value)> = sqlx::query_as(&format!(
        "SELECT id, body FROM \"{dst_schema_pg}\".docs ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3, "all source docs must reach the pg sink");

    // Row 1: Decimal128 must surface as the canonical string `1.230`
    // (the BsonObject → Json arm uses `bson_value::bson_to_json` which
    // pins this form).
    let (id1, ref body1) = rows[0];
    assert_eq!(id1, 1);
    assert_eq!(body1["name"], serde_json::Value::String("alice".into()));
    assert_eq!(
        body1["amount"],
        serde_json::Value::String("1.230".into()),
        "Decimal128 must round-trip as canonical decimal string in JSON"
    );
    assert_eq!(
        body1["tags"],
        serde_json::json!(["a", "b"]),
        "BSON arrays must round-trip as JSON arrays"
    );

    // Row 2: ObjectId must surface as 24-hex.
    let (id2, ref body2) = rows[1];
    assert_eq!(id2, 2);
    assert_eq!(
        body2["oid"],
        serde_json::Value::String("654f10800102030405000001".into()),
        "ObjectId must round-trip as 24-hex string in JSON"
    );
    assert_eq!(
        body2["nested"]["city"],
        serde_json::Value::String("Berlin".into())
    );

    // Row 3: plain int field round-trips as a JSON number.
    let (id3, ref body3) = rows[2];
    assert_eq!(id3, 3);
    assert_eq!(body3["score"], serde_json::json!(42));

    pg.pool.close().await;
}
