//! End-to-end tests for the `mongo-cdc` source.
//!
//! Requires a replica-set Mongo deployment — change streams cannot run
//! on a standalone mongod. `mongo_rs_pool` honours
//! `AIR_ELT_TEST_MONGO_RS_URL` if set (CI uses this) and otherwise spins
//! up a `mongo:8 --replSet rs0` container via testcontainers and runs
//! `replSetInitiate` once.
//!
//! ## Determinism: probe-watch + post-batch resume token (PBRT)
//!
//! A `change_stream` only delivers events that arrive *after* its
//! server-side cursor opens. The naive pattern (`spawn(read_batch)` +
//! `sleep(N ms)` + writes) is racey under CI load. Instead we open a
//! short-lived probe `coll.watch()` synchronously in the test, capture
//! its post-batch resume token (PBRT) — which marks the cluster's
//! current oplog position — drop the probe, do the writes, then call
//! `source.read_batch(.., cursor = Some(PBRT))`. The source's own watch
//! resumes from PBRT, so every test write is in the visibility window
//! regardless of when the source's `watch().await` actually runs.
//! No sleeps, no spawn.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use air_elt_commons_mongodb::bson_value;
use air_elt_commons_testing::mongo::{MongoTestHandle, mongo_rs_pool};
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::model::{CursorFieldValue, CursorState, ReadSpec, RowOp, WriteSpec};
use air_elt_core::traits::{Sink, Source, Storage};
use air_elt_core::types::Value;
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use air_elt_source_mongo_cdc::{MongoCdcSource, MongoCdcSourceConfig};
use air_elt_storage_postgres::{PgStorage, PgStorageConfig};
use bson::{Document, doc};
use futures::stream::TryStreamExt;
use mongodb::Collection;
use sqlx::Executor;

const RESUME_TOKEN_FIELD: &str = "__resume_token";

async fn cdc_source(handle: &MongoTestHandle) -> Arc<MongoCdcSource> {
    let cfg = MongoCdcSourceConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        max_await_time: Some(Duration::from_millis(200)),
        ..Default::default()
    };
    let s = MongoCdcSource::connect("mongo_cdc".into(), cfg)
        .await
        .expect("connect mongo-cdc");
    Arc::new(s)
}

fn cdc_spec(table: &str, limit: usize, mode: &str) -> ReadSpec {
    let mut opts = toml::Table::new();
    opts.insert("mode".into(), toml::Value::String(mode.into()));
    ReadSpec {
        columns: vec!["_id".into(), "name".into()],
        table: table.into(),
        cursor_fields: vec![],
        cursor_order: CursorOrder::Asc,
        limit,
        source_options: opts,
        needs_body: false,
    }
}

async fn enable_pre_post_images(handle: &MongoTestHandle, coll: &str) {
    handle
        .client
        .database(&handle.database)
        .run_command(doc! {
            "collMod": coll,
            "changeStreamPreAndPostImages": { "enabled": true },
        })
        .await
        .expect("enable changeStreamPreAndPostImages");
}

/// Open a probe `watch()` purely to grab a post-batch resume token, then
/// drop it. After the returned `CursorState` is captured, every event
/// emitted by `coll` is visible when `read_batch(.., Some(&cursor))`
/// resumes from it.
async fn capture_pbrt(coll: &Collection<Document>) -> CursorState {
    let stream = coll.watch().await.expect("probe watch open");
    let token = stream
        .resume_token()
        .expect("post-batch resume token must be available right after watch open");
    drop(stream);
    let bson = bson::to_bson(&token).expect("serialise token");
    let v = bson_value::from_bson(&bson).expect("decode token");
    CursorState::new(vec![CursorFieldValue {
        name: RESUME_TOKEN_FIELD.into(),
        value: v,
    }])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_emits_upsert_for_inserts_replace_and_delete() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "users";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 4, "post-image");
    source.validate_access(&spec).await.expect("validate");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.insert_one(doc! { "_id": 1, "name": "alice" })
        .await
        .unwrap();
    coll.insert_one(doc! { "_id": 2, "name": "bob" })
        .await
        .unwrap();
    // Replace shares the upsert arm with insert — covered here as a
    // ride-along to keep the suite small.
    coll.replace_one(doc! { "_id": 2 }, doc! { "_id": 2, "name": "bob2" })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 1 }).await.unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    // BSON int literals (`doc!{"_id": 1}`) encode as Int32, not Int64.
    // `bson_value::from_bson` round-trips that to `Value::Int32`. Sinks
    // widen on the write path; source-side assertions stay honest.
    //
    // Dedup-by-`_id` collapses the four events to two: for _id=1 the
    // insert is shadowed by the later delete; for _id=2 the insert is
    // shadowed by the later replace. Survivors keep their relative
    // chronological order.
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].op, RowOp::Upsert);
    assert_eq!(batch.rows[0].values[0], Value::Int32(2));
    assert_eq!(batch.rows[0].values[1], Value::Text("bob2".into()));
    assert_eq!(batch.rows[1].op, RowOp::Delete);
    assert_eq!(batch.rows[1].values[0], Value::Int32(1));
    // Delete event carries documentKey only — non-key columns must be
    // Null and the row must have full column arity.
    assert_eq!(batch.rows[1].values.len(), spec.columns.len());
    assert_eq!(batch.rows[1].values[1], Value::Null);
    assert!(batch.next_cursor.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_lookup_on_update_attaches_post_image_via_find() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "items";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 7, "name": "before" })
        .await
        .expect("seed");

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 1, "lookup-on-update");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.update_one(doc! { "_id": 7 }, doc! { "$set": { "name": "after" } })
        .await
        .unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].op, RowOp::Upsert);
    assert_eq!(batch.rows[0].values[0], Value::Int32(7));
    // The lookup find must have attached the post-image.
    assert_eq!(batch.rows[0].values[1], Value::Text("after".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_lookup_on_update_skips_when_doc_deleted_between_event_and_find() {
    // Negative-control on the LookupOnUpdate path: when the document is
    // deleted between the change event and our batch-find, the source
    // must warn-and-skip instead of erroring. The follow-up `delete`
    // event surfaces in the next batch.
    let mongo = mongo_rs_pool().await;
    let coll_name = "items_deleted";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 9, "name": "soon-gone" })
        .await
        .expect("seed");

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 2, "lookup-on-update");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    // Update + delete before read_batch issues its lookup.
    coll.update_one(doc! { "_id": 9 }, doc! { "$set": { "name": "changed" } })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 9 }).await.unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    // The update event is dropped (warn-and-skip), only the delete
    // event survives.
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].op, RowOp::Delete);
    assert_eq!(batch.rows[0].values[0], Value::Int32(9));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_post_image_collapses_multiple_updates_per_id() {
    // Multiple updates of the same `_id` in one batch collapse to a
    // single Upsert row carrying the chronologically-last post-image.
    let mongo = mongo_rs_pool().await;
    let coll_name = "multi_updates";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 1, "name": "init" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 4, "post-image");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    // Four updates of the same `_id` so the read_batch fast-path
    // (`events.len() >= spec.limit`) trips and the loop exits without
    // hitting the 30s idle-drain deadline. The collapse semantic still
    // holds: dedup-by-`_id` last-wins → one row carrying the final
    // post-image ("d").
    coll.update_one(doc! { "_id": 1 }, doc! { "$set": { "name": "a" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": 1 }, doc! { "$set": { "name": "b" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": 1 }, doc! { "$set": { "name": "c" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": 1 }, doc! { "$set": { "name": "d" } })
        .await
        .unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].op, RowOp::Upsert);
    assert_eq!(batch.rows[0].values[1], Value::Text("d".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_post_image_insert_then_delete_emits_only_delete() {
    // insert(_id) + delete(_id) in one batch must collapse to a
    // single Delete — not Upsert + Delete (which would otherwise
    // require strict insert-before-delete ordering at the sink).
    let mongo = mongo_rs_pool().await;
    let coll_name = "insert_then_delete";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    // `collMod` requires the collection to exist; seed an unrelated
    // doc first so `enable_pre_post_images` doesn't hit NamespaceNotFound.
    coll.insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 4, "post-image");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    // Two ephemeral _ids, each an insert+delete pair → 4 events so the
    // read_batch fast-path (`events.len() >= spec.limit`) trips. Per-id
    // last-wins collapses each pair to a single Delete; the test
    // semantic ("insert+delete in one batch must surface only as
    // Delete") is preserved across both ids.
    coll.insert_one(doc! { "_id": 5, "name": "ephemeral" })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 5 }).await.unwrap();
    coll.insert_one(doc! { "_id": 6, "name": "ephemeral2" })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 6 }).await.unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    assert_eq!(batch.rows.len(), 2);
    assert!(batch.rows.iter().all(|r| r.op == RowOp::Delete));
    let mut ids: Vec<&Value> = batch.rows.iter().map(|r| &r.values[0]).collect();
    ids.sort_by_key(|v| match v {
        Value::Int32(n) => *n,
        _ => i32::MAX,
    });
    assert_eq!(ids, vec![&Value::Int32(5), &Value::Int32(6)]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_lookup_on_update_collapses_multiple_updates_to_single_row() {
    // Same as the post-image case but exercising the LookupOnUpdate
    // path: dedup happens before `find`, so we end up with one row
    // carrying the final post-image fetched by lookup.
    let mongo = mongo_rs_pool().await;
    let coll_name = "lookup_multi_updates";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 3, "name": "before" })
        .await
        .expect("seed");

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 4, "lookup-on-update");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    // Four updates of the same `_id` so the read_batch fast-path trips
    // on `events.len() >= spec.limit`. Dedup-before-lookup still
    // collapses to a single `find` over `_id=3`, and the row carries
    // the chronologically-last post-image ("final").
    coll.update_one(doc! { "_id": 3 }, doc! { "$set": { "name": "v1" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": 3 }, doc! { "$set": { "name": "v2" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": 3 }, doc! { "$set": { "name": "v3" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": 3 }, doc! { "$set": { "name": "final" } })
        .await
        .unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].op, RowOp::Upsert);
    assert_eq!(batch.rows[0].values[1], Value::Text("final".into()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_drop_collection_surfaces_runtime_error() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "to_drop";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 1 }).await.expect("seed");

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 1, "post-image");
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.drop().await.unwrap();

    let err = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect_err("drop must surface as RuntimeError");
    let msg = err.to_string();
    assert!(
        msg.contains("invalidated") || msg.contains("Drop") || msg.contains("Invalidate"),
        "expected drop/invalidate marker in error, got: {msg}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_to_pg_sink_round_trip_with_inserts_and_deletes() {
    let mongo = mongo_rs_pool().await;
    let pg = pg_pool().await;
    pg.pool
        .execute("CREATE TABLE users (id BIGINT PRIMARY KEY, name TEXT)")
        .await
        .expect("create");

    let coll_name = "users_cdc";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let read_spec = cdc_spec(coll_name, 4, "post-image");
    let src_ctx = source.build_context(&read_spec).await.expect("ctx");

    let sink = PgSink::connect(PgSinkConfig {
        url: pg.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("sink");
    let write_spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.users", pg.schema),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    let sink_ctx = sink.build_context(&write_spec).await.expect("sink ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.insert_one(doc! { "_id": 11, "name": "first" })
        .await
        .unwrap();
    coll.insert_one(doc! { "_id": 12, "name": "second" })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 11 }).await.unwrap();
    coll.delete_one(doc! { "_id": 12 }).await.unwrap();

    let batch = source
        .read_batch(&read_spec, &src_ctx, Some(&cursor))
        .await
        .expect("read");
    // The source dedups by `_id`, last-event-wins. insert(11) +
    // delete(11) collapses to one Delete; same for 12 — so the
    // batch carries exactly two Delete rows.
    assert_eq!(batch.rows.len(), 2);
    assert!(batch.rows.iter().all(|r| r.op == RowOp::Delete));
    sink.write_batch(&write_spec, &sink_ctx, batch, false)
        .await
        .expect("mixed batch");

    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pg.pool)
        .await
        .unwrap();
    assert_eq!(
        count.0, 0,
        "upsert→delete in one batch must land as absent (sink applies upsert first, delete second)"
    );
    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_describe_schema_unifies_heterogeneous_bson() {
    // Mirror of `crates/sources/mongodb/tests/heterogeneous_union_e2e.rs`
    // shape: int32 + int64 in the same field widens to Int64; int + float
    // widens to Float64. Confirms the cdc source delegates to
    // `commons-mongodb::sampling::describe_collection_schema` rather than
    // diverging.
    let mongo = mongo_rs_pool().await;
    let coll_name = "het";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 1_i32, "n": 1_i32, "x": 1_i32 })
        .await
        .unwrap();
    coll.insert_one(doc! { "_id": 2_i64, "n": 2_i64, "x": 1.5_f64 })
        .await
        .unwrap();

    let source = cdc_source(&mongo).await;
    let schema = source
        .describe_schema(coll_name)
        .await
        .expect("describe_schema");
    let n = schema.find("n").expect("n field");
    let x = schema.find("x").expect("x field");
    use air_elt_core::types::DataType;
    assert_eq!(n.data_type, DataType::Int64);
    assert_eq!(x.data_type, DataType::Float64);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_token_round_trips_through_pg_storage_with_reopen() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "tokens";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 0 }).await.expect("seed");

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 1, "post-image");
    let ctx = source.build_context(&spec).await.expect("ctx");
    let cursor_before_writes = capture_pbrt(&coll).await;
    coll.insert_one(doc! { "_id": 100 }).await.unwrap();
    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor_before_writes))
        .await
        .expect("read");
    let cursor = batch.next_cursor.expect("resume token");

    let pg = pg_pool().await;
    let pg_url = pg.url_with_search_path();
    let storage = PgStorage::connect(PgStorageConfig {
        url: pg_url.clone(),
        ..Default::default()
    })
    .await
    .expect("storage");
    storage.migrate().await.expect("migrate");

    let token_json = serde_json::to_value(&cursor).expect("serialise");
    storage
        .save_resume_token("flow_a", &token_json, false)
        .await
        .expect("save");

    // Persistence-across-reopen: drop the storage handle and connect
    // fresh — the row must survive.
    drop(storage);
    let storage2 = PgStorage::connect(PgStorageConfig {
        url: pg_url,
        ..Default::default()
    })
    .await
    .expect("storage reopen");
    let loaded = storage2
        .load_resume_token("flow_a")
        .await
        .expect("load")
        .expect("present");
    assert_eq!(loaded, token_json);
    assert!(
        storage2
            .load_resume_token("flow_b")
            .await
            .unwrap()
            .is_none(),
        "unknown flow → None"
    );
    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_delete_access_real_db_succeeds_for_owner() {
    // The pipeline's mocked test asserts the gating logic; this one
    // checks the real pg `validate_delete_access` actually executes the
    // probe DELETE end-to-end without modifying any rows.
    let pg = pg_pool().await;
    pg.pool
        .execute("CREATE TABLE access_probe (id BIGINT PRIMARY KEY)")
        .await
        .expect("create");
    pg.pool
        .execute("INSERT INTO access_probe (id) VALUES (1), (2), (3)")
        .await
        .expect("seed");

    let sink = PgSink::connect(PgSinkConfig {
        url: pg.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect");
    let spec = WriteSpec {
        columns: vec!["id".into()],
        table: format!("{}.access_probe", pg.schema),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };

    sink.validate_delete_access(&spec)
        .await
        .expect("delete probe");
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM access_probe")
        .fetch_one(&pg.pool)
        .await
        .unwrap();
    assert_eq!(
        count.0, 3,
        "validate_delete_access must roll back — no rows touched"
    );
    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_to_mysql_sink_round_trip_with_inserts_and_deletes() {
    use air_elt_commons_testing::mysql::mysql_pool;
    use air_elt_sink_mysql::{MySqlSink, MySqlSinkConfig};

    let mongo = mongo_rs_pool().await;
    let mysql = mysql_pool().await;
    sqlx::query("CREATE TABLE users_mc (id BIGINT PRIMARY KEY, name VARCHAR(64))")
        .execute(&mysql.pool)
        .await
        .expect("create");

    let coll_name = "users_mysql_cdc";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let read_spec = cdc_spec(coll_name, 4, "post-image");
    let src_ctx = source.build_context(&read_spec).await.expect("ctx");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: mysql.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("sink");
    let write_spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: "users_mc".into(),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    let sink_ctx = sink.build_context(&write_spec).await.expect("sink ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.insert_one(doc! { "_id": 21, "name": "alpha" })
        .await
        .unwrap();
    coll.insert_one(doc! { "_id": 22, "name": "beta" })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 21 }).await.unwrap();
    coll.delete_one(doc! { "_id": 22 }).await.unwrap();

    let batch = source
        .read_batch(&read_spec, &src_ctx, Some(&cursor))
        .await
        .expect("read");
    sink.write_batch(&write_spec, &sink_ctx, batch, false)
        .await
        .expect("mixed batch");
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users_mc")
        .fetch_one(&mysql.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 0);
    mysql.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_to_mongo_sink_round_trip_with_inserts_and_deletes() {
    use air_elt_commons_testing::mongo::mongo_pool;
    use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};

    let mongo = mongo_rs_pool().await;
    let sink_handle = mongo_pool().await;

    let coll_name = "users_mongo_cdc";
    let src_coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    src_coll
        .insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let read_spec = cdc_spec(coll_name, 3, "post-image");
    let src_ctx = source.build_context(&read_spec).await.expect("ctx");

    let sink = MongoSink::connect(MongoSinkConfig {
        url: sink_handle.url.clone(),
        database: Some(sink_handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("sink");
    let write_spec = WriteSpec {
        columns: vec!["_id".into(), "name".into()],
        table: "users_target".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    let sink_ctx = sink.build_context(&write_spec).await.expect("sink ctx");

    let cursor = capture_pbrt(&src_coll).await;
    src_coll
        .insert_one(doc! { "_id": 31, "name": "x" })
        .await
        .unwrap();
    src_coll
        .insert_one(doc! { "_id": 32, "name": "y" })
        .await
        .unwrap();
    src_coll.delete_one(doc! { "_id": 31 }).await.unwrap();

    let batch = source
        .read_batch(&read_spec, &src_ctx, Some(&cursor))
        .await
        .expect("read");
    sink.write_batch(&write_spec, &sink_ctx, batch, false)
        .await
        .expect("mixed batch");

    let target = sink_handle
        .client
        .database(&sink_handle.database)
        .collection::<Document>("users_target");
    let remaining: Vec<Document> = target
        .find(doc! {})
        .await
        .unwrap()
        .try_collect()
        .await
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].get_i32("_id").unwrap(), 32);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_delete_access_pg_fails_for_role_without_delete_privilege() {
    // Negative-control: a pg role with INSERT/SELECT but no DELETE
    // privilege must be rejected at validate-time, not at the first
    // delete batch in the runner. Uses the libpq `options=-c role=...`
    // startup trick to switch role per-connection without manufacturing
    // a separate auth principal.
    let pg = pg_pool().await;
    let role = format!("temp_no_delete_{}", &pg.schema[..8.min(pg.schema.len())]);
    pg.pool
        .execute(format!("CREATE ROLE \"{role}\"").as_str())
        .await
        .expect("create role");
    pg.pool
        .execute(format!("GRANT USAGE ON SCHEMA \"{}\" TO \"{role}\"", pg.schema).as_str())
        .await
        .expect("grant usage");
    pg.pool
        .execute("CREATE TABLE no_delete_probe (id BIGINT PRIMARY KEY)")
        .await
        .expect("create");
    pg.pool
        .execute(
            format!(
                "GRANT SELECT, INSERT ON TABLE \"{}\".no_delete_probe TO \"{role}\"",
                pg.schema
            )
            .as_str(),
        )
        .await
        .expect("grant select/insert");

    let separator = if pg.url.contains('?') { '&' } else { '?' };
    let role_url = format!(
        "{}{separator}options=-c%20role%3D{role}%20-c%20search_path%3D{}",
        pg.url, pg.schema
    );
    let sink = PgSink::connect(PgSinkConfig {
        url: role_url,
        ..Default::default()
    })
    .await
    .expect("connect");
    let spec = WriteSpec {
        columns: vec!["id".into()],
        table: format!("{}.no_delete_probe", pg.schema),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };

    sink.validate_access(&spec)
        .await
        .expect("INSERT path must succeed (role has INSERT)");
    let err = sink
        .validate_delete_access(&spec)
        .await
        .expect_err("DELETE probe must fail");
    let msg = err.to_string();
    assert!(
        msg.to_lowercase().contains("delete"),
        "error must reference DELETE, got: {msg}"
    );

    // Cleanup: schema cascade drops the table on test teardown but the
    // role is global. Drop it explicitly so re-runs don't accumulate
    // dangling roles.
    drop(sink);
    pg.pool
        .execute(
            format!(
                "REVOKE ALL ON TABLE \"{}\".no_delete_probe FROM \"{role}\"",
                pg.schema
            )
            .as_str(),
        )
        .await
        .ok();
    pg.pool
        .execute(format!("REVOKE ALL ON SCHEMA \"{}\" FROM \"{role}\"", pg.schema).as_str())
        .await
        .ok();
    pg.pool
        .execute(format!("DROP ROLE IF EXISTS \"{role}\"").as_str())
        .await
        .ok();
    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_delete_access_mysql_fails_for_user_without_delete_privilege() {
    use air_elt_commons_testing::mysql::mysql_pool;
    use air_elt_sink_mysql::{MySqlSink, MySqlSinkConfig};

    let mysql = mysql_pool().await;
    sqlx::query("CREATE TABLE no_delete_probe (id BIGINT PRIMARY KEY)")
        .execute(&mysql.pool)
        .await
        .expect("create");

    // Stable per-handle suffix so re-runs don't collide.
    let suffix: String = mysql
        .url_with_database()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .rev()
        .take(8)
        .collect();
    let user = format!("aelt_nd_{suffix}");
    let pwd = "probe-pwd";
    sqlx::query(&format!("CREATE USER '{user}'@'%' IDENTIFIED BY '{pwd}'"))
        .execute(&mysql.pool)
        .await
        .expect("create user");
    sqlx::query(&format!(
        "GRANT SELECT, INSERT ON `{}`.no_delete_probe TO '{user}'@'%'",
        extract_db(&mysql.url_with_database()).expect("db"),
    ))
    .execute(&mysql.pool)
    .await
    .expect("grant");

    let limited_url = swap_mysql_credentials(&mysql.url_with_database(), &user, pwd);
    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: limited_url,
        ..Default::default()
    })
    .await
    .expect("connect");
    let spec = WriteSpec {
        columns: vec!["id".into()],
        table: "no_delete_probe".into(),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };

    sink.validate_access(&spec).await.expect("INSERT path ok");
    let err = sink
        .validate_delete_access(&spec)
        .await
        .expect_err("DELETE probe must fail");
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("denied") || msg.contains("delete") || msg.contains("privilege"),
        "expected denial/privilege error, got: {msg}"
    );

    drop(sink);
    sqlx::query(&format!("DROP USER IF EXISTS '{user}'@'%'"))
        .execute(&mysql.pool)
        .await
        .ok();
    mysql.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_validate_access_succeeds_on_not_yet_existing_collection() {
    // Mongo creates collections implicitly. `validate_access` issues a
    // ping + a tiny `find().limit(1)`; the find must not error on a
    // namespace that has no documents yet — operators should be able
    // to point a CDC flow at a future-collection without bootstrapping
    // a placeholder document first.
    let mongo = mongo_rs_pool().await;
    let source = cdc_source(&mongo).await;
    let spec = cdc_spec("not_yet_existing", 1, "post-image");
    source
        .validate_access(&spec)
        .await
        .expect("validate_access on missing collection must not error");
}

fn extract_db(url: &str) -> Option<&str> {
    url.rsplit_once('/')
        .map(|(_, db)| db.split('?').next().unwrap_or(db))
}

/// Replace `user[:pass]` in a `scheme://user[:pass]@host[:port]/db` URL.
/// Test-only; not robust against query-string credentials or IPv6 hosts.
fn swap_mysql_credentials(url: &str, user: &str, pwd: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("scheme");
    let host_part = match rest.split_once('@') {
        Some((_, after)) => after,
        None => rest,
    };
    format!("{scheme}://{user}:{pwd}@{host_part}")
}

// Removed: `cdc_stale_resume_token_surfaces_runtime_error`.
// We tried to assert that a synthetic past-oplog-window token surfaces
// as `ChangeStreamHistoryLost` (code 286). In practice the server
// gracefully accepts a far-past `_data` and resumes from the oldest
// available oplog entry — the resulting batch is "fresh", not an
// error. There is no deterministic way to exhaust the oplog inside a
// unit test (it would require flooding writes against a small-oplog
// node), so we drop the test rather than ship a flaky one. The
// happy-path resume is exercised by `cdc_to_pg_sink_round_trip_…`.

/// Body-fill cost guard — CDC arm. With `needs_body=true` upsert
/// events (insert / replace / update post-image) must populate
/// `Row.body` with the post-image as a
/// `Value::Custom(BsonObjectValue(Document))`. Delete events also
/// carry an (empty) `Value::Custom(BsonObjectValue(Document::new()))`
/// so `Transform::Body` always sees a value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_attaches_body_for_upsert_when_needs_body_set() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "raw_users";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let mut spec = cdc_spec(coll_name, 2, "post-image");
    spec.needs_body = true;
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.insert_one(doc! { "_id": 1, "name": "alice", "extra": 7_i32 })
        .await
        .unwrap();
    coll.delete_one(doc! { "_id": 1 }).await.unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    assert!(!batch.rows.is_empty());
    for row in &batch.rows {
        let v = row
            .body
            .clone()
            .expect("needs_body=true must attach a body on every CDC row");
        assert!(
            matches!(v, air_elt_core::types::Value::Custom(_)),
            "expected Value::Custom(BsonObjectValue), got {v:?}"
        );
    }
}

/// Cost-guard regression at the CDC source: with `needs_body=false`
/// upsert events do NOT populate `Row.body`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cdc_skips_body_for_upsert_when_needs_body_unset() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "no_raw_users";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    coll.insert_one(doc! { "_id": 0, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 1, "post-image");
    assert!(
        !spec.needs_body,
        "default cdc_spec must have needs_body=false"
    );
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    coll.insert_one(doc! { "_id": 1, "name": "alice" })
        .await
        .unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].op, RowOp::Upsert);
    assert!(
        batch.rows[0].body.is_none(),
        "needs_body=false must leave Row.body=None (cost guard)"
    );
}
