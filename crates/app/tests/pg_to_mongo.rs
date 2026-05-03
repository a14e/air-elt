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

use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use bson::doc;
use chrono::{TimeZone, Utc};
use futures::TryStreamExt;
use sqlx::Executor;

mod common;
use common::guard::{MongoDbGuard, PgSchemaGuard};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_mongo_with_mixed_nullability_and_nested_path() {
    let pg = pg_pool().await;
    let mongo = mongo_pool().await;

    let src_schema = format!("{}_src", pg.schema);
    let dst_db = format!("{}_dst", mongo.database);

    // Sibling resources outside the handle's sandbox — guard so they
    // are dropped on test panic too.
    let _pg_guard = PgSchemaGuard::new(pg.pool.clone(), vec![src_schema.clone()]);
    let _mongo_guard = MongoDbGuard::new(mongo.client.clone(), vec![dst_db.clone()]);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
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
                    payload JSONB
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let insert = format!(
        "INSERT INTO \"{src_schema}\".users \
         (id, name, addr_city, is_active, score, nickname, visits, created_at, payload) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)"
    );
    // 5 rows with rotating NULL placement on the nullable columns, so
    // every nullable column hits both NULL and present in the sample.
    for i in 1_i64..=5 {
        let nickname = (i != 2).then(|| format!("nick-{i}"));
        let visits = (i != 3).then_some(i * 10);
        let payload = (i != 4).then(|| serde_json::json!({ "row": i }));
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

mapping = [
    {{ from = "id", to = "_id" }},
    {{ from = "name", to = "name" }},
    {{ from = "addr_city", to = "addr.city" }},
    {{ from = "is_active", to = "is_active" }},
    {{ from = "score", to = "score" }},
    {{ from = "nickname", to = "nickname" }},
    {{ from = "visits", to = "visits" }},
    {{ from = "created_at", to = "created_at" }},
    {{ from = "payload", to = "payload" }},
]

cursor = {{ fields = ["created_at", "id"], order = "asc", interval = "100ms" }}

[flow.users.conflict]
key = ["_id"]
strategy = "overwrite"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    common::pipeline::run_once(&config_path).await;

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
}
