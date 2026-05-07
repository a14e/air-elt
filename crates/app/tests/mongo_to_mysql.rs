//! Cross-vendor: MongoDB source → MySQL sink, MongoDB storage.
//!
//! Proves the runner can pull from a schemaless document source into a
//! schemaful relational sink and surface bugs in:
//!   * sample-based BSON schema inference for many BSON types
//!     (Int32, Int64, Double, Boolean, String, ObjectId-as-binary,
//!     DateTime, Document/Json),
//!   * NULL passthrough across the boundary (Mongo `null`/missing
//!     fields → SQL `NULL` on a nullable column),
//!   * dot-notation source path (`addr.city`) flattening a nested BSON
//!     subtree onto a flat MySQL column,
//!   * `truncate = true` opt-in for unbounded text → `VARCHAR(N)`,
//!   * `ON DUPLICATE KEY UPDATE` upsert on the cursor key, idempotent
//!     across re-runs,
//!   * cursor state lives in the schemaless Mongo storage collection
//!     (`air_elt_cursors`) — the SQL backend never sees it.
//!
//! Sink columns mix nullable + NOT NULL deliberately. Mongo's sample-
//! based inference always emits `nullable = true` (sampling is non-
//! exhaustive), so any NOT NULL sink column needs a `default = "..."`
//! on its mapping — that's the documented bridge across the
//! nullability mismatch (`check_mapping` admits the pair only when a
//! default literal is present). Both shapes (with and without
//! `default`) exercise the runtime `default` substitution path.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_commons_testing::mysql::mysql_pool;
use bson::doc;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_to_mysql_with_many_types_and_nulls() {
    let mongo = mongo_pool().await;
    let mysql = mysql_pool().await;

    let src_db_mongo = format!("{}_src", mongo.database);
    let state_db_mongo = format!("{}_state", mongo.database);
    let dst_db_sql = format!("{}_dst", mysql.schema);

    mysql
        .pool
        .execute(format!("CREATE DATABASE `{dst_db_sql}`").as_str())
        .await
        .unwrap();
    mysql
        .pool
        .execute(
            format!(
                // Mix of NOT NULL and NULL columns. NOT NULL columns must
                // be paired with `default = "..."` on their mapping to
                // bridge the Mongo source's always-nullable inference.
                "CREATE TABLE `{dst_db_sql}`.users (
                    id BIGINT NOT NULL PRIMARY KEY,
                    name VARCHAR(64) NOT NULL,
                    city VARCHAR(64) NOT NULL,
                    score INT NOT NULL,
                    rating DOUBLE NULL,
                    is_active TINYINT(1) NULL,
                    created_at TIMESTAMP NULL,
                    payload JSON NULL
                ) ENGINE=InnoDB"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Seed Mongo source with 5 docs that exercise every BSON type the
    // mapping reads, plus rotating NULL placement on the nullable fields
    // (every column hits at least one NULL within the sample).
    let src_users = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("users");
    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let mut docs = Vec::new();
    for i in 1_i64..=5 {
        let mut d = doc! {
            "_id": i,
            "name": format!("user-{i}"),
            "addr": { "city": format!("city-{i}") },
            "score": i as i32 * 10,
            "rating": (i as f64) * 1.5,
            "is_active": i % 2 == 0,
            "created_at": bson::DateTime::from_millis((base + chrono::Duration::seconds(i)).timestamp_millis()),
            "payload": doc! { "row": i, "next": i + 1 },
        };
        // Rotate NULLs across both NOT NULL columns (which must be
        // bridged via `default = ...`) and genuinely nullable columns
        // (which round-trip as SQL NULL).
        match i {
            2 => {
                // NOT NULL `name` → `default = "anonymous"`.
                d.insert("name", bson::Bson::Null);
                // Nullable `rating` → SQL NULL.
                d.insert("rating", bson::Bson::Null);
            }
            3 => {
                // NOT NULL `city` (via dot-path) → `default = "unknown"`.
                d.insert("addr", doc! { "city": bson::Bson::Null });
                // Nullable `is_active` → SQL NULL.
                d.insert("is_active", bson::Bson::Null);
            }
            4 => {
                // NOT NULL `score` → `default = -1`.
                d.insert("score", bson::Bson::Null);
                d.insert("payload", bson::Bson::Null);
            }
            _ => {}
        }
        docs.push(d);
    }
    src_users.insert_many(docs).await.unwrap();

    let mongo_url = mongo.url.clone();
    let mysql_url = mysql.url_with_database();

    let config_toml = format!(
        r#"
[[sources]]
name = "mongo_src"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{src_db_mongo}" }}

[[sinks]]
name = "mysql_sink"
type = "mysql"
config = {{ url = "{mysql_url}" }}

[[storages]]
name = "mongo_state"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{state_db_mongo}" }}

[flow.users]
source = "mongo_src"
sink = "mysql_sink"
storage = "mongo_state"
from = "users"
to = "{dst_db_sql}.users"
batch-limit = 2

mapping = [
    # `_id` is invariably present in Mongo; on the SQL side `id BIGINT
    # NOT NULL PRIMARY KEY` requires a default to placate the
    # nullable-source check. `0` is a sentinel that should never fire
    # in practice — a missing `_id` would be a malformed document.
    {{ from = "_id", to = "id", default = 0 }},
    # NOT NULL columns with `default = "..."` — `check_mapping` admits
    # `nullable_src → not_null_sink` only when a default literal is
    # present. The runtime `convert` arm substitutes the default when
    # the source value is `Null`, so the seed below intentionally drops
    # `name` / `city` / `score` to NULL on selected rows to verify the
    # substitution actually fires.
    #
    # `truncate = true` is also needed for the text columns: Mongo
    # strings are unbounded text; MySQL `VARCHAR(64)` carries a width.
    # The lossless matrix forbids unbounded -> bounded narrowing — opt
    # into the wider matrix (`is_compatible_with_truncate`).
    {{ from = "name", to = "name", truncate = true, default = "anonymous" }},
    {{ from = "addr.city", to = "city", truncate = true, default = "unknown" }},
    {{ from = "score", to = "score", default = -1 }},
    # Genuinely nullable columns — NULL passes through as SQL NULL.
    {{ from = "rating", to = "rating" }},
    {{ from = "is_active", to = "is_active" }},
    {{ from = "created_at", to = "created_at" }},
    {{ from = "payload", to = "payload" }},
]

cursor = {{ fields = ["_id"], order = "asc", interval = "100ms" }}

[flow.users.conflict]
key = ["id"]
strategy = "overwrite"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // NOT NULL columns are non-Option in the SELECT — sqlx will panic
    // if any row has a SQL NULL there, which would itself be the bug
    // we want to catch.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        String,
        String,
        i32,
        Option<f64>,
        Option<bool>,
        Option<DateTime<Utc>>,
        Option<serde_json::Value>,
    )> = sqlx::query_as(&format!(
        "SELECT id, name, city, score, rating, is_active, created_at, payload \
         FROM `{dst_db_sql}`.users ORDER BY id"
    ))
    .fetch_all(&mysql.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5, "all 5 docs must land in the SQL sink");

    let r1 = &rows[0];
    assert_eq!(r1.0, 1);
    assert_eq!(r1.1, "user-1");
    assert_eq!(r1.2, "city-1");
    assert_eq!(r1.3, 10);
    assert_eq!(r1.4, Some(1.5));
    assert_eq!(r1.5, Some(false));
    assert_eq!(r1.6, Some(base + chrono::Duration::seconds(1)));
    assert_eq!(r1.7, Some(serde_json::json!({"row": 1, "next": 2})));

    // Defaults must fire on the NOT NULL columns where the source was NULL.
    assert_eq!(rows[1].1, "anonymous", "row 2 → name default substituted");
    assert_eq!(rows[2].2, "unknown", "row 3 → city default substituted");
    assert_eq!(rows[3].3, -1, "row 4 → score default substituted");

    // Genuinely nullable columns round-trip as SQL NULL where the source had it.
    assert!(rows[1].4.is_none(), "row 2 → rating NULL");
    assert!(rows[2].5.is_none(), "row 3 → is_active NULL");
    assert!(rows[3].7.is_none(), "row 4 → payload NULL");

    // Re-run is a no-op thanks to the upsert. Verify both row count
    // and content survived — a buggy upsert that wiped substituted
    // defaults back to NULL would surface here.
    app.run_once().await.expect("run_once (re-run)");
    let rows2: Vec<(i64, String, i32)> = sqlx::query_as(&format!(
        "SELECT id, name, score FROM `{dst_db_sql}`.users ORDER BY id"
    ))
    .fetch_all(&mysql.pool)
    .await
    .unwrap();
    assert_eq!(rows2.len(), 5, "upsert must keep the row count stable");
    assert_eq!(rows2[1].1, "anonymous", "default kept after re-run");
    assert_eq!(rows2[3].2, -1, "default kept after re-run");

    // Cursor lives in the Mongo storage collection.
    let state = mongo
        .client
        .database(&state_db_mongo)
        .collection::<bson::Document>("air_elt_cursors");
    let cursor_doc = state
        .find_one(doc! { "_id": "users" })
        .await
        .unwrap()
        .expect("cursor saved");
    assert!(cursor_doc.get("cursor").is_some());

    mysql.pool.close().await;
}
