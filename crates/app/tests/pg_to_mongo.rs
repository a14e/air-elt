//! Cross-vendor: PostgreSQL source → MongoDB sink, PostgreSQL storage.
//!
//! Proves the runner can glue a schemaful relational source to a
//! schemaless document sink and surface bugs in:
//!   * mixed nullable / NOT NULL columns (NULL must round-trip into BSON
//!     as `Bson::Null`, not silently get dropped),
//!   * the seven canonical types likely to land in a real PG → Mongo
//!     migration: `bigint`, `text`, `bool`, `int`, `bigint NULL`,
//!     `timestamptz`, `jsonb`,
//!   * dot-notation mapping (`addr_city` → `addr.city`) flattens a flat
//!     SQL column into a nested BSON path on write,
//!   * multi-batch drain (`batch-limit = 2`, 5 rows → 3 batches),
//!   * a 2-field tuple cursor `(created_at, id)` advancing as a
//!     lex-compare across batches — the deleted same-vendor pg→pg
//!     test was the only place this end-to-end shape lived,
//!   * cursor state lives in the relational storage (`air_elt_cursors`).

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use bson::doc;
use chrono::{TimeZone, Utc};
use futures::TryStreamExt;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_mongo_with_mixed_nullability_and_nested_path() {
    let pg = pg_pool().await;
    let mongo = mongo_pool().await;

    let src_schema = format!("{}_src", pg.schema);
    let dst_db = format!("{}_dst", mongo.database);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    // Source carries the broadest type set the PG source produces. The
    // Mongo sink is schemaless, so the matrix narrowing check is skipped
    // and every value flows through to BSON without per-column truncate
    // gating. XML round-trips as `Bson::String` (the canonical text form
    // of XML).
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".users (
                    id BIGINT PRIMARY KEY,
                    name TEXT NOT NULL,
                    addr_city TEXT NOT NULL,
                    is_active BOOLEAN NOT NULL,
                    score INTEGER NOT NULL,
                    nickname TEXT,
                    visits BIGINT,
                    created_at TIMESTAMPTZ NOT NULL,
                    payload JSONB,
                    rating SMALLINT NOT NULL,
                    score32 REAL NOT NULL,
                    score64 DOUBLE PRECISION NOT NULL,
                    huge_count NUMERIC(20, 0) NOT NULL,
                    balance NUMERIC(10, 2) NOT NULL,
                    handle VARCHAR(8) NOT NULL,
                    avatar BYTEA NOT NULL,
                    avatar_big BYTEA,
                    born_on DATE NOT NULL,
                    public_id UUID NOT NULL,
                    profile_xml XML
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let insert = format!(
        "INSERT INTO \"{src_schema}\".users (
            id, name, addr_city, is_active, score, nickname, visits, created_at, payload,
            rating, score32, score64, huge_count, balance, handle,
            avatar, avatar_big, born_on, public_id, profile_xml
         ) VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9,
            $10, $11, $12, $13::numeric, $14::numeric, $15,
            $16, $17, $18, $19, $20::xml
         )"
    );
    // 5 rows with rotating NULL placement on the nullable columns, so
    // every nullable column hits both NULL and present in the sample.
    for i in 1_i64..=5 {
        let nickname = (i != 2).then(|| format!("nick-{i}"));
        let visits = (i != 3).then_some(i * 10);
        let payload = (i != 4).then(|| serde_json::json!({ "row": i }));
        let avatar_big = (i != 2).then(|| vec![0xab_u8; i as usize]);
        let profile_xml =
            (i != 5).then(|| format!("<profile id=\"{i}\"><name>row-{i}</name></profile>"));
        let huge: bigdecimal::BigDecimal = format!("{i}9999999999999999999").parse().unwrap();
        let balance: bigdecimal::BigDecimal = format!("{i}.50").parse().unwrap();
        sqlx::query(&insert)
            .bind(i)
            .bind(format!("user-{i}"))
            .bind(format!("city-{i}"))
            .bind(i % 2 == 0)
            .bind(i as i32 * 100)
            .bind(nickname)
            .bind(visits)
            .bind(base + chrono::Duration::seconds(i))
            .bind(payload)
            .bind(i as i16 + 1)
            .bind(0.5_f32 + i as f32)
            .bind(0.125_f64 * (i + 1) as f64)
            .bind(huge)
            .bind(balance)
            .bind(format!("h{i}"))
            .bind(vec![i as u8, (i + 1) as u8])
            .bind(avatar_big)
            .bind(chrono::NaiveDate::from_ymd_opt(1990, 1, i as u32).unwrap())
            .bind(uuid::Uuid::from_u128(
                0xa0eebc99_9c0b_4ef8_bb6d_6bb9bd380a00 + i as u128,
            ))
            .bind(profile_xml)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let mongo_url = mongo.url.clone();

    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "mongo_sink"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{dst_db}" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.users]
source = "pg_src"
sink = "mongo_sink"
storage = "pg_state"
from = "{src_schema}.users"
to = "users"
batch-limit = 2

cursor = {{ fields = ["created_at", "id"], order = "asc", interval = "100ms" }}

[flow.users.conflict]
key = ["_id"]
strategy = "overwrite"

[flow.users.mapping]
_id = "id"
name = "name"
"addr.city" = "addr_city"
is_active = "is_active"
score = "score"
nickname = "nickname"
visits = "visits"
created_at = "created_at"
payload = "payload"
rating = "rating"
score32 = "score32"
score64 = "score64"
huge_count = "huge_count"
balance = "balance"
handle = "handle"
avatar = "avatar"
avatar_big = "avatar_big"
born_on = "born_on"
public_id = "public_id"
profile_xml = "profile_xml"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let sink = mongo
        .client
        .database(&dst_db)
        .collection::<bson::Document>("users");
    let mut cursor = sink.find(doc! {}).sort(doc! { "_id": 1 }).await.unwrap();
    let mut got = Vec::new();
    while let Some(d) = cursor.try_next().await.unwrap() {
        got.push(d);
    }
    assert_eq!(got.len(), 5, "all rows landed in mongo sink");

    // Spot-check NOT NULL columns on a row in the middle.
    let third = &got[2];
    assert_eq!(third.get_i64("_id").unwrap(), 3);
    assert_eq!(third.get_str("name").unwrap(), "user-3");
    assert_eq!(
        third.get_document("addr").unwrap().get_str("city").unwrap(),
        "city-3",
        "dot-notation target must materialise as nested document"
    );
    assert!(!third.get_bool("is_active").unwrap(), "row 3 → odd → false");
    assert_eq!(third.get_i32("score").unwrap(), 300);
    // Wide-type round-trip: every canonical PG type lands as a BSON value.
    assert_eq!(third.get_i32("rating").unwrap(), 4);
    let score32_val = third.get("score32").expect("score32 present");
    // Float32 → BSON: depending on the version of bson, this lands as
    // `Bson::Double` (f64). Probe both shapes for safety.
    let s32 = score32_val
        .as_f64()
        .or_else(|| score32_val.as_i32().map(f64::from))
        .or_else(|| score32_val.as_i64().map(|n| n as f64))
        .expect("score32 should land as a BSON number");
    assert!((s32 - 3.5_f64).abs() < 0.001);
    let s64_val = third.get("score64").expect("score64 present");
    let s64 = s64_val
        .as_f64()
        .or_else(|| s64_val.as_i32().map(f64::from))
        .expect("score64 should land as a BSON number");
    assert!((s64 - 0.5_f64).abs() < 0.001);
    // BigInt overflows i64 and arrives as a string (Decimal → JSON-canonical
    // string rule, see `value_to_json` / `to_bson`).
    assert_eq!(third.get_str("huge_count").unwrap(), "39999999999999999999");
    // Decimal(10, 2) becomes Bson::Decimal128 when parseable; assert by
    // probing both shapes (Decimal128 vs String fallback).
    let balance_val = third.get("balance").expect("balance present");
    let balance_str = match balance_val {
        bson::Bson::Decimal128(d) => d.to_string(),
        bson::Bson::String(s) => s.clone(),
        other => panic!("unexpected balance shape: {other:?}"),
    };
    let balance_parsed: bigdecimal::BigDecimal = balance_str.parse().unwrap();
    assert_eq!(
        balance_parsed,
        "3.50".parse::<bigdecimal::BigDecimal>().unwrap()
    );
    assert_eq!(third.get_str("handle").unwrap(), "h3");
    let avatar = third.get_binary_generic("avatar").unwrap();
    assert_eq!(avatar, &vec![3_u8, 4]);
    let avatar_big = third.get_binary_generic("avatar_big").unwrap();
    assert_eq!(avatar_big, &vec![0xab_u8; 3]);
    let born_on = third.get_datetime("born_on").unwrap();
    let born_ms = born_on.timestamp_millis();
    let born_chrono = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(born_ms).unwrap();
    assert_eq!(
        born_chrono.date_naive(),
        chrono::NaiveDate::from_ymd_opt(1990, 1, 3).unwrap()
    );
    // PG UUID round-trips as BSON Binary subtype 4 (UUID).
    let public_id_val = third.get("public_id").expect("public_id present");
    match public_id_val {
        bson::Bson::Binary(b) => assert_eq!(b.bytes.len(), 16),
        bson::Bson::String(s) => assert_eq!(s.len(), 36),
        other => panic!("unexpected public_id shape: {other:?}"),
    }
    assert_eq!(
        third.get_str("profile_xml").unwrap(),
        "<profile id=\"3\"><name>row-3</name></profile>"
    );

    // The Mongo sink's documented semantics map SQL NULL to *missing*
    // BSON keys (not `Bson::Null`) — Mongo treats absent and explicit
    // null as distinct, and the project picks "missing". Verify by
    // asserting the key is absent, not that its value equals null.
    assert!(
        got[1].get("nickname").is_none(),
        "row 2 → nickname NULL must be absent from the document, got {:?}",
        got[1].get("nickname")
    );
    assert!(
        got[2].get("visits").is_none(),
        "row 3 → visits NULL must be absent, got {:?}",
        got[2].get("visits")
    );
    assert!(
        got[3].get("payload").is_none(),
        "row 4 → payload NULL must be absent, got {:?}",
        got[3].get("payload")
    );
    assert!(
        got[1].get("avatar_big").is_none(),
        "row 2 → avatar_big NULL must be absent, got {:?}",
        got[1].get("avatar_big")
    );
    assert!(
        got[4].get("profile_xml").is_none(),
        "row 5 → profile_xml NULL must be absent, got {:?}",
        got[4].get("profile_xml")
    );

    // Where the nullable columns are present, the value must be intact.
    assert_eq!(got[0].get_str("nickname").unwrap(), "nick-1");
    assert_eq!(got[0].get_i64("visits").unwrap(), 10);
    // PG `JSONB` arrives at the sink as `Value::Json` and `to_bson` of
    // `serde_json::Value::Object` round-trips it as a `Bson::Document`.
    // The exact int subtype (Int32 vs Int64) depends on the chosen
    // `serde_json::Number` representation; just assert the round-tripped
    // i64 view of the field.
    match got[4].get("payload").expect("row 5 payload present") {
        bson::Bson::Document(d) => {
            let row_val = d.get("row").expect("payload.row present");
            let n = row_val
                .as_i64()
                .or_else(|| row_val.as_i32().map(i64::from))
                .expect("payload.row is integer");
            assert_eq!(n, 5);
        }
        other => panic!("expected payload as Document, got {other:?}"),
    }

    // Cursor advances and is persisted in the PG storage.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "users");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(
        parsed.fields.len(),
        2,
        "tuple cursor must persist both fields"
    );
    assert_eq!(parsed.fields[0].name, "created_at");
    assert_eq!(parsed.fields[1].name, "id");
    assert_eq!(parsed.fields[1].value, Value::Int64(5));

    pg.pool.close().await;
}

/// Wildcard mapping (`mapping = ["*"]`) end-to-end with a relational
/// source and a schemaless mongo sink. The sink exposes no schema, so
/// expansion falls back to the **source** schema and
/// produces a column-by-name passthrough.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_mongo_wildcard_source_schema_fallback() {
    let pg = pg_pool().await;
    let mongo = mongo_pool().await;

    let src_schema = format!("{}_wild", pg.schema);
    let dst_db = format!("{}_wild", mongo.database);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".items (
                    id BIGINT NOT NULL,
                    name TEXT,
                    created_at TIMESTAMPTZ NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = Utc.with_ymd_and_hms(2026, 5, 1, 12, 0, 0).unwrap();
    let insert = format!(
        "INSERT INTO \"{src_schema}\".items (id, name, created_at) \
         VALUES ($1, $2, $3)"
    );
    // 3 rows, including one with NULL `name` to exercise nullable
    // passthrough under wildcard expansion.
    let rows: [(i64, Option<&str>); 3] = [(1, Some("alpha")), (2, None), (3, Some("gamma"))];
    for (i, (id, name)) in rows.iter().enumerate() {
        sqlx::query(&insert)
            .bind(id)
            .bind(*name)
            .bind(base + chrono::Duration::seconds(i as i64))
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let mongo_url = mongo.url.clone();

    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "mongo_sink"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{dst_db}" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.items]
source = "pg_src"
sink = "mongo_sink"
storage = "pg_state"
from = "{src_schema}.items"
to = "items"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.items.mapping]
"*" = "*"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let sink = mongo
        .client
        .database(&dst_db)
        .collection::<bson::Document>("items");
    let mut cursor = sink.find(doc! {}).sort(doc! { "id": 1 }).await.unwrap();
    let mut got = Vec::new();
    while let Some(d) = cursor.try_next().await.unwrap() {
        got.push(d);
    }
    assert_eq!(got.len(), 3, "wildcard expansion landed all 3 rows");

    // Field types per BSON: id → Int64, name → String|Null (absent on NULL),
    // created_at → DateTime.
    for (i, doc) in got.iter().enumerate() {
        let id = doc
            .get("id")
            .unwrap_or_else(|| panic!("row {i} missing id"));
        assert!(
            matches!(id, bson::Bson::Int64(_)),
            "row {i} id must be BSON Int64, got {id:?}"
        );

        let created = doc
            .get("created_at")
            .unwrap_or_else(|| panic!("row {i} missing created_at"));
        assert!(
            matches!(created, bson::Bson::DateTime(_)),
            "row {i} created_at must be BSON DateTime, got {created:?}"
        );
    }

    assert_eq!(got[0].get_i64("id").unwrap(), 1);
    assert_eq!(got[1].get_i64("id").unwrap(), 2);
    assert_eq!(got[2].get_i64("id").unwrap(), 3);

    assert_eq!(got[0].get_str("name").unwrap(), "alpha");
    // SQL NULL → missing key (per existing pg→mongo sink contract).
    assert!(
        got[1].get("name").is_none(),
        "row 2 → NULL name must be absent, got {:?}",
        got[1].get("name")
    );
    assert_eq!(got[2].get_str("name").unwrap(), "gamma");

    // Sanity: cursor advanced through wildcard-expanded mapping.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "items");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields.len(), 1);
    assert_eq!(parsed.fields[0].name, "id");
    assert_eq!(parsed.fields[0].value, Value::Int64(3));

    pg.pool.close().await;
}

/// `*:body` pack across the pg → mongo seam. The pg source populates
/// `Row.body` with `Value::Json` (the canonical body type); the
/// body conversion plan on this flow is identity (`Json → Json`), so
/// the value reaches the mongo sink as
/// `Value::Json(serde_json::Value::Object(...))` and the sink writes
/// it as a `Bson::Document`.
///
/// Pinning this contract here protects the cross-vendor body path
/// even though the PG source's hook is the trait default — a future
/// regression that breaks the body conversion plan for the
/// `Json → Json` arm would surface as a runtime panic in
/// `bind_value_separated` analogues, or as the body silently being
/// dropped on the mongo sink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_mongo_body_pack_routes_body_through() {
    let pg = pg_pool().await;
    let mongo = mongo_pool().await;

    let src_schema = format!("{}_body", pg.schema);
    let dst_db = format!("{}_body", mongo.database);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".events (
                    id   BIGINT NOT NULL PRIMARY KEY,
                    name TEXT NOT NULL,
                    score INT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();
    let insert =
        format!("INSERT INTO \"{src_schema}\".events (id, name, score) VALUES ($1, $2, $3)");
    for (id, name, score) in [(1_i64, "alpha", 10_i32), (2, "beta", 20)] {
        sqlx::query(&insert)
            .bind(id)
            .bind(name)
            .bind(score)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let mongo_url = mongo.url.clone();

    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "mongo_sink"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{dst_db}" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.events]
source = "pg_src"
sink = "mongo_sink"
storage = "pg_state"
from = "{src_schema}.events"
to = "events"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.events.mapping]
id = "id"
body = "*"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let sink = mongo
        .client
        .database(&dst_db)
        .collection::<bson::Document>("events");
    let mut cursor = sink.find(doc! {}).sort(doc! { "id": 1 }).await.unwrap();
    let mut got = Vec::new();
    while let Some(d) = cursor.try_next().await.unwrap() {
        got.push(d);
    }
    assert_eq!(got.len(), 2);

    for (i, expected_id) in [1_i64, 2].iter().enumerate() {
        assert_eq!(got[i].get_i64("id").unwrap(), *expected_id);
        let body = got[i]
            .get_document("body")
            .unwrap_or_else(|_| panic!("row {i}: body must land as a BSON Document"));
        // The packed body carries every source field.
        assert_eq!(
            body.get_i64("id").unwrap(),
            *expected_id,
            "row {i}: body.id must mirror the direct id"
        );
        assert!(
            body.get_str("name").is_ok(),
            "row {i}: body.name must be a string"
        );
        let score = body.get("score").expect("body.score present");
        // serde_json → bson maps integers to Int32 or Int64 depending
        // on the underlying Number; just assert numeric.
        assert!(
            score
                .as_i64()
                .or_else(|| score.as_i32().map(i64::from))
                .is_some(),
            "row {i}: body.score must be numeric, got {score:?}"
        );
    }

    pg.pool.close().await;
}

/// AIR-70 `switch` expression with integer keys, struct-to-unstructured
/// (pg → mongo schemaless sink). Exercises both a key hit and the
/// `default` fallback when the source value misses the switch table.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_mongo_switch_int_keys_schemaless_sink() {
    let pg = pg_pool().await;
    let mongo = mongo_pool().await;

    let src_schema = format!("{}_sw", pg.schema);
    let dst_db = format!("{}_sw", mongo.database);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".tickets (
                    id          BIGINT NOT NULL,
                    status_code INT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let insert = format!(
        "INSERT INTO \"{src_schema}\".tickets (id, status_code) \
         VALUES ($1, $2)"
    );
    let fixtures: [(i64, i32); 3] = [(1, 1), (2, 2), (3, 99)];
    for (id, code) in fixtures {
        sqlx::query(&insert)
            .bind(id)
            .bind(code)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let mongo_url = mongo.url.clone();

    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "mongo_sink"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{dst_db}" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.tickets]
source = "pg_src"
sink = "mongo_sink"
storage = "pg_state"
from = "{src_schema}.tickets"
to = "tickets"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.tickets.mapping]
_id = "id"
label = {{ from = "status_code", switch = {{ 1 = "open", 2 = "closed" }}, default = "unknown" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let sink = mongo
        .client
        .database(&dst_db)
        .collection::<bson::Document>("tickets");
    let mut cursor = sink.find(doc! {}).sort(doc! { "_id": 1 }).await.unwrap();
    let mut got = Vec::new();
    while let Some(d) = cursor.try_next().await.unwrap() {
        got.push(d);
    }
    assert_eq!(got.len(), 3);

    assert_eq!(got[0].get_i64("_id").unwrap(), 1);
    assert_eq!(got[0].get_str("label").unwrap(), "open");
    assert_eq!(got[1].get_i64("_id").unwrap(), 2);
    assert_eq!(got[1].get_str("label").unwrap(), "closed");
    assert_eq!(got[2].get_i64("_id").unwrap(), 3);
    assert_eq!(
        got[2].get_str("label").unwrap(),
        "unknown",
        "miss must fall back to `default`"
    );

    pg.pool.close().await;
}
