//! Cross-vendor: MongoDB source → PostgreSQL sink, PostgreSQL storage.
//!
//! The original test guarded the BSON-body → JSONB conversion arm.
//! It now also exercises **broad typed-mapping coverage**: every
//! canonical type the BSON-side codec emits as a typed `Value` (Bool,
//! Int32, Int64, Float64, String, Binary generic, BSON DateTime,
//! Document, ObjectId) lands in a typed PG sink column and round-trips
//! through the matrix.
//!
//! Highlights:
//! * `truncate = true` on `String → varchar(N)` and on `Float64 →
//!   Float32`.
//! * Dot-notation source path (`addr.city`) flattens a nested BSON
//!   sub-document onto a flat PG column.
//! * Same `oid` (`MongoObjectIdType` custom) feeds two sink columns —
//!   `varchar(24)` (hex form) and `bytea` (12-byte form).
//! * NOT NULL sink columns are bridged with `default = ...` literals
//!   (Mongo sample inference always emits `nullable = true`); nullable
//!   sink columns let actual SQL NULLs round-trip.
//!
//! Intentionally not covered:
//! * `Binary subtype=UUID → uuid`: `bson_value::from_bson` returns
//!   `Value::Bytes` regardless of subtype while the inferrer reports
//!   `DataType::Uuid` — documented MVP limitation that would need a
//!   source-side fix to bridge.
//! * `BSON DateTime → date` (Timestamp→Date with `truncate=true`):
//!   `compatibility::CompatibilityValidator` re-checks the post-Convert
//!   output type against the sink under truncate and rejects `Date↔Date`
//!   under truncate. The matrix accepts the conversion itself, but the
//!   second-pass identity check is the blocker.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_commons_testing::pg::pg_pool;
use bson::{Bson, doc, oid::ObjectId, spec::BinarySubtype};
use chrono::{DateTime, TimeZone, Utc};
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_pg_broad_type_coverage() {
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
                // Mix of NOT NULL and NULL columns. Every NOT NULL column
                // is paired with `default = ...` on its mapping below to
                // bridge Mongo's always-nullable inference.
                "CREATE TABLE \"{dst_schema_pg}\".docs (
                    id          bigint PRIMARY KEY,
                    name        varchar(64) NOT NULL,
                    city        varchar(64) NOT NULL,
                    score       integer NOT NULL,
                    qty         bigint NULL,
                    rating      double precision NULL,
                    rating32    real NULL,
                    flag        boolean NULL,
                    note        text NULL,
                    note_short  varchar(8) NULL,
                    blob        bytea NULL,
                    oid_hex     varchar(24) NULL,
                    oid_bytes   bytea NULL,
                    created_at  timestamptz NULL,
                    payload     jsonb NULL,
                    body        jsonb NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Seed source docs that exercise every BSON variant we care about.
    let src = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("docs");

    let oid = ObjectId::from_bytes([
        0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
    ]);
    let blob_bytes = vec![0xDE, 0xAD, 0xBE, 0xEFu8];
    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();

    // Row 1: every field present, every nullable column carries a value.
    let doc_1 = doc! {
        "_id": 1_i64,
        "name": "alice",
        "addr": { "city": "Berlin" },
        "score": 10_i32,
        "qty": 1_000_000_i64,
        "rating": 1.5_f64,
        // `rating_short` carries a value that fits Float32 to verify
        // the truncate path doesn't visibly degrade in-range values.
        "rating_short": 2.5_f64,
        "flag": true,
        "note": "hello",
        // `note_long` carries text longer than the varchar(8) sink
        // column so the runtime truncation path actually fires.
        "note_long": "this string is way longer than eight chars",
        "blob": Bson::Binary(bson::Binary {
            subtype: BinarySubtype::Generic,
            bytes: blob_bytes.clone(),
        }),
        "oid": oid,
        "created_at": bson::DateTime::from_millis(base.timestamp_millis()),
        "payload": doc! { "row": 1_i32, "tags": ["a", "b"] },
    };

    // Row 2: rotate NULLs through every nullable sink column and trip
    // the `default` substitution on every NOT NULL sink column.
    let doc_2 = doc! {
        "_id": 2_i64,
        // NULL on NOT NULL `name` -> default "anonymous" must fire.
        "name": Bson::Null,
        // NULL on NOT NULL `city` (via dot-path) -> default "unknown".
        "addr": { "city": Bson::Null },
        // NULL on NOT NULL `score` -> default -1.
        "score": Bson::Null,
        // Every nullable column nulled out so the NULL passthrough is
        // exercised at least once per column within the sample.
        "qty": Bson::Null,
        "rating": Bson::Null,
        "rating_short": Bson::Null,
        "flag": Bson::Null,
        "note": Bson::Null,
        "note_long": Bson::Null,
        "blob": Bson::Null,
        "oid": Bson::Null,
        "created_at": Bson::Null,
        "payload": Bson::Null,
    };

    for d in [&doc_1, &doc_2] {
        src.insert_one(d.clone()).await.unwrap();
    }

    let mongo_url = mongo.url.clone();
    let pg_url = pg.url_with_search_path();

    // The mapping covers every canonical type:
    //   * Bool (`flag`), Int32 (`score`), Int64 (`_id`, `qty`),
    //     Float64 (`rating`), Float64→Float32 truncate (`rating_short`).
    //   * Text unbounded → text (`note`); Text unbounded → varchar(N)
    //     with truncate (`name`, `note_short`, dotted `city`).
    //   * Bytes generic → bytea (`blob`).
    //   * BSON DateTime → timestamptz (`created_at`).
    //   * BSON Document → jsonb (`payload`).
    //   * ObjectId (custom MongoObjectIdType) → varchar(24) (hex form)
    //     AND → bytea (12-byte form), same `oid` feeding two sinks.
    //   * `body = "*"` packs the whole document as JSONB so we exercise
    //     the body-pack arm next to the typed columns.
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

cursor = {{ fields = ["_id"], order = "asc", interval = "100ms" }}

[flow.docs.mapping]
id          = {{ from = "_id", default = 0 }}
name        = {{ from = "name", truncate = true, default = "anonymous" }}
city        = {{ from = "addr.city", truncate = true, default = "unknown" }}
score       = {{ from = "score", default = -1 }}
qty         = "qty"
rating      = "rating"
rating32    = {{ from = "rating_short", truncate = true }}
flag        = "flag"
note        = "note"
note_short  = {{ from = "note_long", truncate = true }}
blob        = "blob"
oid_hex     = "oid"
oid_bytes   = "oid"
created_at  = "created_at"
payload     = "payload"
body        = "*"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // Read everything back, ordered by id. Split into two SELECT
    // queries because sqlx's `FromRow` is only implemented for tuples
    // of arity ≤ 16.
    #[allow(clippy::type_complexity)]
    let head: Vec<(
        i64,            // id
        String,         // name
        String,         // city
        i32,            // score
        Option<i64>,    // qty
        Option<f64>,    // rating
        Option<f32>,    // rating32
        Option<bool>,   // flag
        Option<String>, // note
        Option<String>, // note_short
    )> = sqlx::query_as(&format!(
        "SELECT id, name, city, score, qty, rating, rating32, flag, note, note_short \
         FROM \"{dst_schema_pg}\".docs ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    #[allow(clippy::type_complexity)]
    let tail: Vec<(
        i64,                       // id
        Option<Vec<u8>>,           // blob
        Option<String>,            // oid_hex
        Option<Vec<u8>>,           // oid_bytes
        Option<DateTime<Utc>>,     // created_at
        Option<serde_json::Value>, // payload
        Option<serde_json::Value>, // body
    )> = sqlx::query_as(&format!(
        "SELECT id, blob, oid_hex, oid_bytes, created_at, payload, body \
         FROM \"{dst_schema_pg}\".docs ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(head.len(), 2, "all source docs must reach the pg sink");
    assert_eq!(tail.len(), 2);

    // ---------- Row 1: every field populated ----------
    let h1 = &head[0];
    let t1 = &tail[0];
    assert_eq!(h1.0, 1);
    assert_eq!(h1.1, "alice");
    assert_eq!(h1.2, "Berlin");
    assert_eq!(h1.3, 10);
    assert_eq!(h1.4, Some(1_000_000));
    assert_eq!(h1.5, Some(1.5));
    assert_eq!(h1.6, Some(2.5_f32));
    assert_eq!(h1.7, Some(true));
    assert_eq!(h1.8.as_deref(), Some("hello"));
    // truncate path: the source text length > 8 chars; sink must hold
    // only the first 8 UTF-8 chars.
    assert_eq!(h1.9.as_deref(), Some("this str"));
    assert_eq!(t1.1.as_deref(), Some(blob_bytes.as_slice()));
    // ObjectId → 24-hex string.
    assert_eq!(t1.2.as_deref(), Some("654f10800102030405000001"));
    // Same ObjectId → 12-byte payload.
    assert_eq!(t1.3.as_deref(), Some(oid.bytes().as_slice()));
    assert_eq!(t1.4, Some(base));
    assert_eq!(
        t1.5,
        Some(serde_json::json!({ "row": 1, "tags": ["a", "b"] }))
    );
    let body_1 = t1.6.as_ref().expect("body present on row 1");
    assert_eq!(body_1["name"], serde_json::Value::String("alice".into()));
    assert_eq!(
        body_1["oid"],
        serde_json::Value::String("654f10800102030405000001".into())
    );

    // ---------- Row 2: defaults + NULL passthrough ----------
    let h2 = &head[1];
    let t2 = &tail[1];
    assert_eq!(h2.0, 2);
    assert_eq!(h2.1, "anonymous", "NOT NULL name -> default fired");
    assert_eq!(h2.2, "unknown", "NOT NULL city -> default fired");
    assert_eq!(h2.3, -1, "NOT NULL score -> default fired");
    assert!(h2.4.is_none(), "nullable qty round-trips as NULL");
    assert!(h2.5.is_none(), "nullable rating round-trips as NULL");
    assert!(h2.6.is_none(), "nullable rating32 round-trips as NULL");
    assert!(h2.7.is_none(), "nullable flag round-trips as NULL");
    assert!(h2.8.is_none(), "nullable note round-trips as NULL");
    assert!(h2.9.is_none(), "nullable note_short round-trips as NULL");
    assert!(t2.1.is_none(), "nullable blob round-trips as NULL");
    assert!(t2.2.is_none(), "nullable oid_hex round-trips as NULL");
    assert!(t2.3.is_none(), "nullable oid_bytes round-trips as NULL");
    assert!(t2.4.is_none(), "nullable created_at round-trips as NULL");
    assert!(t2.5.is_none(), "nullable payload round-trips as NULL");

    pg.pool.close().await;
}

/// AIR-70 `switch` expression with string keys, unstructured-to-struct
/// (mongo schemaless source → pg sink). Last row hits the `default`
/// arm because its `status` is not in the switch table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_pg_switch_string_keys_schemaless_source() {
    let mongo = mongo_pool().await;
    let pg = pg_pool().await;

    let src_db_mongo = format!("{}_sw", mongo.database);
    let state_db_mongo = format!("{}_sw_state", mongo.database);
    let dst_schema_pg = format!("{}_sw", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema_pg}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema_pg}\".orders_labelled (
                    id           BIGINT,
                    status_label TEXT
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let src = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("orders");
    // 1, 2 — hits. 3 — miss → default arm. 4 — explicit null. 5 —
    // missing field entirely (schemaless source, the key is just absent
    // in the BSON document).
    //
    // Mongo schemaless source surfaces both "explicit null" and "field
    // absent" as `Value::Null` into the runtime. `TransformOp::Switch`'s
    // NULL-source branch returns the table default — so rows 4 and 5
    // must land as "unknown" too.
    let docs_seed = vec![
        doc! { "_id": 1_i64, "status": "ACTIVE" },
        doc! { "_id": 2_i64, "status": "FINISHED" },
        doc! { "_id": 3_i64, "status": "OTHER" },
        doc! { "_id": 4_i64, "status": bson::Bson::Null },
        doc! { "_id": 5_i64 },
    ];
    for d in &docs_seed {
        src.insert_one(d.clone()).await.unwrap();
    }

    let mongo_url = mongo.url.clone();
    let pg_url = pg.url_with_search_path();

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

[flow.orders]
source = "mongo_src"
sink = "pg_sink"
storage = "mongo_state"
from = "orders"
to = "{dst_schema_pg}.orders_labelled"
batch-limit = 8

cursor = {{ fields = ["_id"], order = "asc", interval = "100ms" }}

[flow.orders.mapping]
id = {{ from = "_id", default = 0 }}
status_label = {{ from = "status", switch = {{ ACTIVE = "active", FINISHED = "finished" }}, default = "unknown" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(&format!(
        "SELECT id, status_label FROM \"{dst_schema_pg}\".orders_labelled ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0], (1, Some("active".to_string())));
    assert_eq!(rows[1], (2, Some("finished".to_string())));
    assert_eq!(
        rows[2],
        (3, Some("unknown".to_string())),
        "miss must fall back to `default`"
    );
    assert_eq!(
        rows[3],
        (4, Some("unknown".to_string())),
        "explicit BSON null source must take the switch default"
    );
    assert_eq!(
        rows[4],
        (5, Some("unknown".to_string())),
        "missing source field (absent in document) must take the switch default"
    );

    pg.pool.close().await;
}

/// AIR-69 schemaless-source heterogeneous-value drift: a single Mongo
/// collection where the same field carries `Int32` in some documents
/// and `Int64` in others. Pre-AIR-69 the sample-derived
/// `ColumnConversionPlan` baked the first variant the sampler saw, so
/// the second variant would blow up at runtime with
/// `ValueShapeMismatch`. With `Source::schemaless = true` the Transform
/// compiler emits a dynamic-source `TransformOp::Convert`
/// (`ColumnConversionPlan.source = None`) and the runtime
/// resolves the source `DataType` from the actual `Value` variant per
/// cell — every doc lands cleanly in the typed PG sink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_pg_heterogeneous_int_widths_drain_successfully() {
    let mongo = mongo_pool().await;
    let pg = pg_pool().await;

    let src_db_mongo = format!("{}_drift", mongo.database);
    let state_db_mongo = format!("{}_drift_state", mongo.database);
    let dst_schema_pg = format!("{}_drift", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema_pg}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                // Sink demands `bigint` — wide enough to admit both
                // Int32 and Int64 BSON values via dynamic dispatch.
                "CREATE TABLE \"{dst_schema_pg}\".metrics (
                    id    bigint PRIMARY KEY,
                    score bigint NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let src = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("metrics");
    // Mix Int32 (BSON `i32`) and Int64 (BSON `i64`) on the same field
    // `score` across documents. The first batch of docs reads with
    // `score` as Int32; the second batch is Int64. Under a static
    // plan the second batch would fail; under dynamic-source Convert
    // (`plan.source = None`) both
    // succeed and round-trip into the `bigint` sink.
    let docs_seed: Vec<bson::Document> = vec![
        doc! { "_id": 1_i64, "score": 10_i32 },
        doc! { "_id": 2_i64, "score": 20_i32 },
        doc! { "_id": 3_i64, "score": 9_000_000_000_i64 },
        doc! { "_id": 4_i64, "score": 30_i32 },
        doc! { "_id": 5_i64, "score": 12_000_000_000_i64 },
    ];
    for d in &docs_seed {
        src.insert_one(d.clone()).await.unwrap();
    }

    let mongo_url = mongo.url.clone();
    let pg_url = pg.url_with_search_path();

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

[flow.metrics]
source = "mongo_src"
sink = "pg_sink"
storage = "mongo_state"
from = "metrics"
to = "{dst_schema_pg}.metrics"
batch-limit = 16

cursor = {{ fields = ["_id"], order = "asc", interval = "100ms" }}

[flow.metrics.mapping]
id = "_id"
score = "score"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once()
        .await
        .expect("run_once with mixed int widths");

    let rows: Vec<(i64, i64)> = sqlx::query_as(&format!(
        "SELECT id, score FROM \"{dst_schema_pg}\".metrics ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0], (1, 10));
    assert_eq!(rows[1], (2, 20));
    assert_eq!(rows[2], (3, 9_000_000_000));
    assert_eq!(rows[3], (4, 30));
    assert_eq!(rows[4], (5, 12_000_000_000));

    pg.pool.close().await;
}

/// Mongo `String` → PG `inet` via `Text → Ipv4/Ipv6` convert.
/// BSON has no IP type — operators store IPs as strings; the Air Elt
/// matrix admits `Text → Ipv4` / `Text → Ipv6` losslessly and binds
/// the typed value into a PG `inet` column.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_pg_ip_string_to_inet() {
    let mongo = mongo_pool().await;
    let pg = pg_pool().await;

    let src_db_mongo = format!("{}_ip", mongo.database);
    let state_db_mongo = format!("{}_ip_state", mongo.database);
    let dst_schema_pg = format!("{}_ip", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema_pg}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema_pg}\".clients (
                    id    BIGINT PRIMARY KEY,
                    v4    INET NOT NULL,
                    v6    INET NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let src = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("clients");
    let docs = vec![
        doc! { "_id": 1_i64, "v4": "192.0.2.1", "v6": "2001:db8::1" },
        doc! { "_id": 2_i64, "v4": "203.0.113.42", "v6": "fe80::1" },
    ];
    for d in &docs {
        src.insert_one(d.clone()).await.unwrap();
    }

    let mongo_url = mongo.url.clone();
    let pg_url = pg.url_with_search_path();
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

[flow.clients]
source = "mongo_src"
sink = "pg_sink"
storage = "mongo_state"
from = "clients"
to = "{dst_schema_pg}.clients"
batch-limit = 8

cursor = {{ fields = ["_id"], order = "asc", interval = "100ms" }}

[flow.clients.mapping]
id = {{ from = "_id", default = 0 }}
v4 = "v4"
v6 = "v6"
"#
    );

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, &config_toml).unwrap();
    let app = App::from_path(&path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, String, String)> = sqlx::query_as(&format!(
        "SELECT id, host(v4)::text, host(v6)::text \
         FROM \"{dst_schema_pg}\".clients ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![
            (1, "192.0.2.1".to_string(), "2001:db8::1".to_string()),
            (2, "203.0.113.42".to_string(), "fe80::1".to_string()),
        ]
    );

    pg.pool.close().await;
}
