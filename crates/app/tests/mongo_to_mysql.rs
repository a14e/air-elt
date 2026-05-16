//! Cross-vendor: MongoDB source → MySQL sink, MongoDB storage.
//!
//! Proves the runner can pull from a schemaless document source into a
//! schemaful relational sink and surface bugs in:
//!   * sample-based BSON schema inference for the BSON types Mongo
//!     hands the codec as a typed `Value` (Int32, Int64, Double,
//!     Boolean, String, Binary generic, DateTime, Document/Json,
//!     ObjectId via the `mongodb.object_id` custom),
//!   * NULL passthrough across the boundary (Mongo `null`/missing
//!     fields → SQL `NULL` on a nullable column),
//!   * dot-notation source path (`addr.city`) flattening a nested BSON
//!     subtree onto a flat MySQL column,
//!   * `truncate = true` opt-in for unbounded text → `VARCHAR(N)`,
//!     unbounded bytes → `VARBINARY(N)`, `Float64 → Float32` and
//!     `Int32 → UInt32` (sign-loss path),
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
use air_elt_core::model::cursor::CursorState;
use air_elt_core::types::value::Value;
use bson::{Bson, doc, oid::ObjectId, spec::BinarySubtype};
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
                    id              BIGINT NOT NULL PRIMARY KEY,
                    name            VARCHAR(64) NOT NULL,
                    city            VARCHAR(64) NOT NULL,
                    score           INT NOT NULL,
                    score_unsigned  INT UNSIGNED NULL,
                    qty             BIGINT NULL,
                    rating          DOUBLE NULL,
                    rating32        FLOAT NULL,
                    is_active       TINYINT(1) NULL,
                    note            TEXT NULL,
                    note_short      VARCHAR(8) NULL,
                    blob_data       VARBINARY(64) NULL,
                    oid_hex         VARCHAR(24) NULL,
                    oid_bytes       VARBINARY(12) NULL,
                    created_at      TIMESTAMP NULL,
                    payload         JSON NULL
                ) ENGINE=InnoDB"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Seed 5 docs that exercise every BSON type the mapping reads,
    // plus rotating NULL placement on the nullable fields (every
    // column hits at least one NULL within the sample).
    let src_users = mongo
        .client
        .database(&src_db_mongo)
        .collection::<bson::Document>("users");
    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let oid_seed = ObjectId::from_bytes([
        0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
    ]);
    let blob_seed: Vec<u8> = vec![0xDE, 0xAD, 0xBE, 0xEFu8];

    let mut docs = Vec::new();
    for i in 1_i64..=5 {
        let mut d = doc! {
            "_id": i,
            "name": format!("user-{i}"),
            "addr": { "city": format!("city-{i}") },
            "score": i as i32 * 10,
            // `score_unsigned` re-reads `score` and routes to a UNSIGNED
            // sink column via `truncate = true` (sign-loss path).
            "qty": i * 100_000,
            "rating": (i as f64) * 1.5,
            // `rating_short` carries an in-range Float64 so the
            // truncate path doesn't visibly degrade the value.
            "rating_short": (i as f64) * 2.5,
            "is_active": i % 2 == 0,
            "note": format!("note-{i}"),
            // `note_long` exceeds the varchar(8) sink width so the
            // text-narrowing path actually fires on row 1.
            "note_long": "this string is way longer than eight chars",
            "blob": Bson::Binary(bson::Binary {
                subtype: BinarySubtype::Generic,
                bytes: blob_seed.clone(),
            }),
            "oid": oid_seed,
            "created_at": bson::DateTime::from_millis((base + chrono::Duration::seconds(i)).timestamp_millis()),
            "payload": doc! { "row": i, "next": i + 1 },
        };
        // Rotate NULLs across NOT NULL columns (which must be bridged
        // via `default = ...`) and genuinely nullable columns (which
        // round-trip as SQL NULL).
        match i {
            2 => {
                // NOT NULL `name` → `default = "anonymous"`.
                d.insert("name", Bson::Null);
                // Nullable `rating` → SQL NULL.
                d.insert("rating", Bson::Null);
                // Nullable `rating32` → SQL NULL.
                d.insert("rating_short", Bson::Null);
                // Nullable `score_unsigned` → SQL NULL.
                d.insert("score", Bson::Null);
            }
            3 => {
                // NOT NULL `city` (via dot-path) → `default = "unknown"`.
                d.insert("addr", doc! { "city": Bson::Null });
                // Nullable `is_active` → SQL NULL.
                d.insert("is_active", Bson::Null);
                // Nullable `qty` → SQL NULL.
                d.insert("qty", Bson::Null);
                // Nullable `note` / `note_short` → SQL NULL.
                d.insert("note", Bson::Null);
                d.insert("note_long", Bson::Null);
            }
            4 => {
                // NOT NULL `score` → `default = -1`.
                d.insert("score", Bson::Null);
                d.insert("payload", Bson::Null);
                d.insert("blob", Bson::Null);
                d.insert("created_at", Bson::Null);
            }
            5 => {
                // Nullable `oid` → SQL NULL on both oid_hex and oid_bytes.
                d.insert("oid", Bson::Null);
            }
            _ => {}
        }
        docs.push(d);
    }
    src_users.insert_many(docs).await.unwrap();

    let mongo_url = mongo.url.clone();
    let mysql_url = mysql.url_with_database();

    // The mapping covers:
    //   * Bool (`is_active`), Int32 (`score`), Int32→UInt32 truncate
    //     (`score_unsigned`), Int64 (`_id`, `qty`), Float64 (`rating`),
    //     Float64→Float32 truncate (`rating32`).
    //   * Text unbounded → varchar(N) with truncate (`name`, dotted
    //     `city`, `note_short`), Text unbounded → text (`note`).
    //   * Bytes unbounded → varbinary(N) with truncate (`blob_data`).
    //   * BSON DateTime → timestamp (`created_at`).
    //   * BSON Document → json (`payload`).
    //   * ObjectId (custom MongoObjectIdType) → varchar(24) (hex form)
    //     AND → varbinary(12) (12-byte form), same `oid` feeding two
    //     sinks lossless.
    let config_yaml = format!(
        r#"
sources:
  - name: mongo_src
    type: mongodb
    config:
      url: "{mongo_url}"
      database: "{src_db_mongo}"

sinks:
  - name: mysql_sink
    type: mysql
    config:
      url: "{mysql_url}"

storages:
  - name: mongo_state
    type: mongodb
    config:
      url: "{mongo_url}"
      database: "{state_db_mongo}"

flow:
  users:
    source: mongo_src
    sink: mysql_sink
    storage: mongo_state
    from: users
    to: "{dst_db_sql}.users"
    batch-limit: 2

    mapping:
      # `_id` is invariably present in Mongo; on the SQL side `id BIGINT
      # NOT NULL PRIMARY KEY` requires a default to placate the
      # nullable-source check. `0` is a sentinel that should never fire
      # in practice -- a missing `_id` would be a malformed document.
      id: {{ from: _id, default: 0 }}
      # NOT NULL columns with `default: ...` -- `check_mapping` admits
      # `nullable_src -> not_null_sink` only when a default literal is
      # present.
      name: {{ from: name, truncate: true, default: anonymous }}
      city: {{ from: "addr.city", truncate: true, default: unknown }}
      score: {{ from: score, default: -1 }}
      # Int32 source → UInt32 sink: matrix forbids without truncate
      # (sign loss). With `truncate: true` the conversion saturates at
      # the unsigned floor when negative; the seed feeds only positives.
      score_unsigned: {{ from: score, truncate: true }}
      qty: qty
      rating: rating
      rating32: {{ from: rating_short, truncate: true }}
      is_active: is_active
      note: {{ from: note, truncate: true }}
      note_short: {{ from: note_long, truncate: true }}
      # Bytes unbounded → bytes(N) requires truncate.
      blob_data: {{ from: blob, truncate: true }}
      # Same ObjectId routed both to hex text and to raw 12 bytes.
      oid_hex: oid
      oid_bytes: oid
      created_at: created_at
      payload: payload

    cursor:
      fields: [_id]
      order: asc
      interval: "100ms"

    conflict:
      key: [id]
      strategy: overwrite
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // sqlx's `FromRow` tops out at 16-arity tuples -- split into two
    // reads keyed on the BIGINT primary key.
    #[allow(clippy::type_complexity)]
    let head: Vec<(
        i64,            // id
        String,         // name
        String,         // city
        i32,            // score
        Option<u32>,    // score_unsigned
        Option<i64>,    // qty
        Option<f64>,    // rating
        Option<f32>,    // rating32
        Option<bool>,   // is_active
        Option<String>, // note
        Option<String>, // note_short
    )> = sqlx::query_as(&format!(
        "SELECT id, name, city, score, score_unsigned, qty, rating, rating32, \
                is_active, note, note_short \
         FROM `{dst_db_sql}`.users ORDER BY id"
    ))
    .fetch_all(&mysql.pool)
    .await
    .unwrap();
    #[allow(clippy::type_complexity)]
    let tail: Vec<(
        i64,                       // id
        Option<Vec<u8>>,           // blob_data
        Option<String>,            // oid_hex
        Option<Vec<u8>>,           // oid_bytes
        Option<DateTime<Utc>>,     // created_at
        Option<serde_json::Value>, // payload
    )> = sqlx::query_as(&format!(
        "SELECT id, blob_data, oid_hex, oid_bytes, created_at, payload \
         FROM `{dst_db_sql}`.users ORDER BY id"
    ))
    .fetch_all(&mysql.pool)
    .await
    .unwrap();
    assert_eq!(head.len(), 5, "all 5 docs must land in the SQL sink");
    assert_eq!(tail.len(), 5);

    // Row 1: every nullable column carries a real value (NULL rotation
    // hits rows 2..=5).
    let h1 = &head[0];
    let t1 = &tail[0];
    assert_eq!(h1.0, 1);
    assert_eq!(h1.1, "user-1");
    assert_eq!(h1.2, "city-1");
    assert_eq!(h1.3, 10);
    assert_eq!(h1.4, Some(10_u32));
    assert_eq!(h1.5, Some(100_000));
    assert_eq!(h1.6, Some(1.5));
    assert_eq!(h1.7, Some(2.5_f32));
    assert_eq!(h1.8, Some(false));
    assert_eq!(h1.9.as_deref(), Some("note-1"));
    // Truncate fired: source text length > 8 chars, sink keeps first 8.
    assert_eq!(h1.10.as_deref(), Some("this str"));
    assert_eq!(t1.1.as_deref(), Some(blob_seed.as_slice()));
    assert_eq!(t1.2.as_deref(), Some("654f10800102030405000001"));
    assert_eq!(t1.3.as_deref(), Some(oid_seed.bytes().as_slice()));
    assert_eq!(t1.4, Some(base + chrono::Duration::seconds(1)));
    assert_eq!(t1.5, Some(serde_json::json!({"row": 1, "next": 2})));

    // Defaults fire on NOT NULL columns where the source value was NULL.
    assert_eq!(head[1].1, "anonymous", "row 2 -> name default fired");
    assert_eq!(head[2].2, "unknown", "row 3 -> city default fired");
    assert_eq!(head[3].3, -1, "row 4 -> score default fired");

    // NULL passthrough on nullable columns where the source had NULL.
    assert!(head[1].6.is_none(), "row 2 -> rating NULL");
    assert!(head[1].7.is_none(), "row 2 -> rating32 NULL");
    assert!(head[1].4.is_none(), "row 2 -> score_unsigned NULL");
    assert!(head[2].8.is_none(), "row 3 -> is_active NULL");
    assert!(head[2].5.is_none(), "row 3 -> qty NULL");
    assert!(head[2].9.is_none(), "row 3 -> note NULL");
    assert!(head[2].10.is_none(), "row 3 -> note_short NULL");
    assert!(tail[3].1.is_none(), "row 4 -> blob NULL");
    assert!(tail[3].4.is_none(), "row 4 -> created_at NULL");
    assert!(tail[3].5.is_none(), "row 4 -> payload NULL");
    assert!(tail[4].2.is_none(), "row 5 -> oid_hex NULL");
    assert!(tail[4].3.is_none(), "row 5 -> oid_bytes NULL");

    // Re-run is a no-op thanks to the upsert. Verify both row count and
    // content survive -- a buggy upsert that wiped substituted defaults
    // back to NULL would surface here.
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
    let cursor_json = cursor_doc
        .get_str("cursor")
        .expect("cursor field is a string");
    let cursor_state: CursorState =
        serde_json::from_str(cursor_json).expect("cursor JSON parses as CursorState");
    assert_eq!(cursor_state.fields.len(), 1, "single cursor field `_id`");
    assert_eq!(cursor_state.fields[0].name, "_id");
    assert_eq!(
        cursor_state.fields[0].value,
        Value::Int64(5),
        "cursor must point at the last `_id` processed"
    );

    mysql.pool.close().await;
}
