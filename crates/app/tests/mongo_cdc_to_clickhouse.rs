//! End-to-end: MongoDB CDC source → ClickHouse sink, MongoDB storage.
//!
//! Wire-through coverage of the append-only sink contract
//! (`Sink::supports_deletes() == false`). The runner no longer
//! pre-filters `RowOp::Delete`; sinks self-filter. Today this contract
//! is only covered by:
//!   * runner-side mocks (`crates/core/src/flow/runner.rs`);
//!   * sink-side unit tests (`crates/sinks/clickhouse/tests/basic_sink.rs`,
//!     `crates/sinks/questdb/tests/corner_cases.rs`).
//!
//! This test exercises the full pipeline end-to-end:
//!   * real CDC source emits insert/delete/insert events,
//!   * runner ships the full batch to the sink,
//!   * sink (ClickHouse) self-filters the Delete,
//!   * the resume token still advances past the dropped event.
//!
//! ClickHouse was picked over QuestDB because reads are synchronous on
//! the MergeTree path — there is no async-WAL polling delay analogous
//! to QuestDB's pg-wire INSERT → WAL-apply gap, so the row-count
//! assertion can be a single fetch with no retry loop.
//!
//! ## Determinism: probe-watch + post-batch resume token (PBRT)
//!
//! A change stream only delivers events that arrive after its cursor
//! opens. Spawning the writes on a background task and racing them
//! against `App::from_path` (validation pipeline + first `read_batch`
//! `coll.watch().await`) is racey under CI load — if validation
//! takes longer than the writer's sleep, events fire before the
//! runner is watching and get lost.
//!
//! Instead we use the PBRT trick documented in
//! `crates/sources/mongo-cdc/tests/e2e.rs`: open a short-lived probe
//! `coll.watch()` BEFORE the test writes, capture its resume token
//! (which marks the cluster's current oplog position), drop the probe,
//! seed that token into `MongoStorage` under the flow's name, do the
//! writes, then call `app.run_once()`. The runner loads the seeded
//! token via `Storage::load_resume_token`, opens its own change stream
//! with `resume_after = PBRT`, and the server replays every event from
//! the seeded position — including the writes we did before the
//! runner started.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_mongodb::bson_value;
use air_elt_commons_testing::clickhouse::clickhouse_handle;
// CDC requires a replica set (`$changeStream` is RS-only). `mongo_rs_pool()`
// honours `AIR_ELT_TEST_MONGO_URL` first, which in CI points at a plain
// standalone Mongo service — that path fails on `coll.watch()` with
// `Location40573`. `mongo_rs_pool()` bypasses the env override and uses
// the dedicated RS container unconditionally, so this test runs against
// the same backend in CI and locally.
use air_elt_commons_testing::mongo::mongo_rs_pool;
use air_elt_core::traits::Storage;
use air_elt_storage_mongodb::{MongoStorage, MongoStorageConfig};
use bson::{Bson, Document, doc};
use mongodb::Collection;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_cdc_to_clickhouse_drops_deletes_and_advances_cursor() {
    let mongo = mongo_rs_pool().await;
    let ch = clickhouse_handle().await;

    // --- Mongo source collection ---
    // Separate database for source-data vs the storage-state database
    // so the two roles are not entangled. Both live on the same RS
    // container, so `mongo.url` (which carries `?replicaSet=rs0&...`)
    // can be reused for storage as well.
    let src_db = format!("{}_src", mongo.database);
    let state_db = format!("{}_state", mongo.database);
    let coll_name = "events";

    let coll = mongo
        .client
        .database(&src_db)
        .collection::<Document>(coll_name);
    // Seed an unrelated doc — `collMod` (for changeStreamPreAndPostImages)
    // requires the collection to exist, and the CDC source's
    // `validate_access` / sampling probes need at least one document
    // to infer a schema.
    coll.insert_one(doc! { "_id": 0_i32, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo.client, &src_db, coll_name).await;

    let flow_name = "events_cdc";

    // --- Capture PBRT, seed it into storage, then do the test writes ---
    //
    // The probe watch opens a cursor at the cluster's current oplog
    // position; its resume token (PBRT) is the "checkpoint" we hand
    // the runner so its own watch resumes from exactly this point.
    // Drop the probe immediately — we only need the token.
    let pbrt_doc = capture_pbrt_doc(&coll).await;

    // Pre-create + seed `MongoStorage` so the runner's first tick loads
    // the PBRT via `Storage::load_resume_token`. The Mongo storage's
    // `migrate()` is a no-op so opening early doesn't change behaviour
    // — the same collection lookup happens whether we open here or
    // later in `app.run_once()`.
    let seed_storage = MongoStorage::connect(
        MongoStorageConfig {
            url: mongo.url.clone(),
            database: Some(state_db.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("seed storage connect");
    seed_storage.migrate().await.expect("seed storage migrate");
    let token_json = bson_doc_to_serde_json(&pbrt_doc);
    seed_storage
        .save_resume_token(flow_name, &token_json, false)
        .await
        .expect("seed resume token");
    drop(seed_storage);

    // 5 events total: insert(1) + insert(2) + insert(3) + delete(2)
    // + insert(4). The mongo-cdc source dedups per-`_id` last-wins, so
    // _id=2's insert is shadowed by its delete; survivors:
    //   1=Upsert("alice"), 2=Delete, 3=Upsert("carol"), 4=Upsert("dave").
    coll.insert_one(doc! { "_id": 1_i32, "name": "alice" })
        .await
        .expect("insert 1");
    coll.insert_one(doc! { "_id": 2_i32, "name": "bob" })
        .await
        .expect("insert 2");
    coll.insert_one(doc! { "_id": 3_i32, "name": "carol" })
        .await
        .expect("insert 3");
    coll.delete_one(doc! { "_id": 2_i32 })
        .await
        .expect("delete 2");
    coll.insert_one(doc! { "_id": 4_i32, "name": "dave" })
        .await
        .expect("insert 4");

    // --- ClickHouse sink table ---
    // `supports_deletes() = false` is declared by the ClickHouse sink
    // unconditionally, regardless of engine. MergeTree is the cheap
    // default for ordered ingestion; ORDER BY tuple() lets us avoid
    // an `allow_nullable_key=1` setting for the `Nullable(Int32)` id.
    // `_id` is nullable in the mongo-cdc inferred schema (the sampling
    // step models every field as potentially missing).
    ch.exec(
        "CREATE TABLE events_sink (
            id Nullable(Int32),
            name Nullable(String)
        ) ENGINE = MergeTree() ORDER BY tuple()",
    )
    .await
    .unwrap();

    // --- Flow config ---
    // mongo-cdc requires the developed source-ref form (`{ name, mode }`)
    // — `mode = "post-image"` matches the pre/post-image enablement on
    // the source collection. No `cursor.fields` (CDC uses a resume
    // token), and no `[flow.x.conflict]` because the sink declares
    // `supports_deletes = false` (the runner accepts append-only ingest
    // without an upsert key in that case).
    let mongo_url = &mongo.url;
    let ch_url = &ch.url;
    let config_toml = format!(
        r#"
[[sources]]
name = "mongo_cdc"
type = "mongo-cdc"
# `max-await-time` caps each server-side change-stream long-poll
# (small value → snappy delivery). `operation-timeout` is the
# whole-batch wallclock budget inside the source's `read_batch`
# loop — a safety net if the deadline arm has to fire. Both kept
# small so a misconfiguration surfaces inside the test budget
# rather than hitting the flow's outer `query-timeout` (default
# 30s).
config = {{ url = "{mongo_url}", database = "{src_db}", max-await-time = "200ms", operation-timeout = "3s" }}

[[sinks]]
name = "ch_sink"
type = "clickhouse"
config = {{ url = "{ch_url}", database = "{ch_db}", user = "default", password = "" }}

[[storages]]
name = "mongo_state"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{state_db}" }}

[flow.{flow_name}]
source = {{ name = "mongo_cdc", mode = "post-image" }}
sink = "ch_sink"
storage = "mongo_state"
from = "{coll_name}"
to = "events_sink"
# Set to the exact count of writes (pre-dedup) we emit below so the
# source's `read_batch` loop exits cleanly once all events are in,
# rather than continuing to `try_next` against a now-quiet stream.
batch-limit = 5
# Sampling validation issues EXPLAIN against the sink to dry-run the
# write; ClickHouse rejects EXPLAIN INSERT, so the Mongo-default
# sampling=true would surface as a sampling failure. Disable it for
# this CDC flow — the contract under test is the wire-through of
# Delete-drop, not validation-time sample coverage.
validation = {{ sampling = false }}

[flow.{flow_name}.mapping]
id = "_id"
name = "name"

[flow.{flow_name}.cursor]
fields = []
order = "asc"
interval = "100ms"
"#,
        ch_db = ch.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // --- Assertions ---
    //
    // 5 CDC events emitted: insert(1) + insert(2) + insert(3) + delete(2)
    //   + insert(4).
    //
    // mongo-cdc dedups per-`_id` last-wins inside a batch: for _id=2 the
    // insert is shadowed by the delete, so the source emits four rows
    // (1=Upsert, 2=Delete, 3=Upsert, 4=Upsert). The append-only sink
    // (`supports_deletes() = false`) then drops the Delete row inside
    // `write_batch`, leaving three Upsert rows landing in CH:
    //   {1, "alice"}, {3, "carol"}, {4, "dave"}.
    //
    // If the runner had pre-dropped the whole batch on seeing a Delete
    // (the old contract), we would see only 0 or 2 rows. If the sink
    // had failed to filter, we would see 4 rows (the Delete would
    // surface as an error or a phantom Null-name row). The number 3
    // is the load-bearing signal.
    let body = ch
        .exec("SELECT id, coalesce(name, '<<NULL>>') FROM events_sink ORDER BY id FORMAT TabSeparated")
        .await
        .expect("select");
    let rows: Vec<&str> = body.trim().split('\n').collect();
    assert_eq!(
        rows.len(),
        3,
        "expected 3 rows in CH (delete dropped): {body}"
    );

    let parsed: Vec<(i32, &str)> = rows
        .iter()
        .map(|r| {
            let mut it = r.split('\t');
            let id: i32 = it.next().unwrap().parse().unwrap();
            let name = it.next().unwrap();
            (id, name)
        })
        .collect();
    assert_eq!(parsed[0], (1, "alice"));
    // _id=2 must NOT be present — the source emitted Delete for it and
    // the append-only sink dropped that row.
    assert!(
        !parsed.iter().any(|(id, _)| *id == 2),
        "deleted _id=2 must NOT land in the sink; got {parsed:?}",
    );
    assert_eq!(parsed[1], (3, "carol"));
    // The post-delete insert must be present — proves the Delete did
    // not accidentally swallow the next event in the same batch.
    assert_eq!(parsed[2], (4, "dave"));

    // --- Cursor advanced past the delete ---
    //
    // The runner saves a resume token after each non-empty batch.
    // The post-run token must differ from the PBRT we seeded: even
    // though the batch contained a Delete that the sink dropped, the
    // cursor still committed (proving the delete didn't abort the
    // commit path).
    let state_storage = MongoStorage::connect(
        MongoStorageConfig {
            url: mongo.url.clone(),
            database: Some(state_db.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("storage reopen");
    let saved = state_storage
        .load_resume_token(flow_name)
        .await
        .expect("load");
    let saved = saved.expect("resume token must be persisted after run_once");
    assert_ne!(
        saved, token_json,
        "resume token must advance past the seeded PBRT — cursor did not commit",
    );
    drop(state_storage);
}

/// Open a probe `watch()` purely to grab a post-batch resume token,
/// then drop it. Returns the BSON document form of the token so the
/// caller can both seed it into storage and compare against the
/// post-run token to assert advancement.
async fn capture_pbrt_doc(coll: &Collection<Document>) -> Document {
    let stream = coll.watch().await.expect("probe watch open");
    let token = stream
        .resume_token()
        .expect("post-batch resume token must be available right after watch open");
    drop(stream);
    let bson = bson::to_bson(&token).expect("serialise token");
    match bson {
        Bson::Document(d) => d,
        other => panic!("resume token must serialise to a BSON document, got {other:?}"),
    }
}

/// Convert the captured BSON resume-token document to the
/// `serde_json::Value` shape `Storage::save_resume_token` expects.
/// Round-trips through `bson_value` to mirror the runner's own
/// `extract_resume_token` path: BSON → `Value::Json` → serde_json.
fn bson_doc_to_serde_json(doc: &Document) -> serde_json::Value {
    let v = bson_value::from_bson(&Bson::Document(doc.clone())).expect("decode token");
    match v {
        air_elt_core::types::Value::Json(j) => j,
        other => panic!("token decode must yield Value::Json, got {other:?}"),
    }
}

async fn enable_pre_post_images(client: &mongodb::Client, db: &str, coll: &str) {
    // collMod requires the collection to exist; the seed insert above
    // guarantees that. Mongo 6+ honours the option; `mongo_rs_pool()`
    // always backs onto a mongo:8 RS container in this repo so we can
    // rely on the option being available.
    client
        .database(db)
        .run_command(doc! {
            "collMod": coll,
            "changeStreamPreAndPostImages": { "enabled": true },
        })
        .await
        .expect("enable changeStreamPreAndPostImages");
}
