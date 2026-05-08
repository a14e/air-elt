//! Versioning e2e for the mongo-sink upsert path.
//!
//! The sink picks one of two implementations at `connect()` based on
//! the connected server version:
//!   - **8.0+**: `Client::bulk_write` (single round-trip per batch).
//!   - **<8.0**: one `update` command via `run_command` carrying every
//!     row's `{ q, u, upsert: true }` entry — also a single round-trip,
//!     supported by every server since 2.6.
//!
//! These tests assert the version is detected correctly *and* that
//! the upsert round-trip works end-to-end on each path. Connection
//! URLs come from `AIR_ELT_TEST_MONGO_URL` (8.0) and
//! `AIR_ELT_TEST_MONGO_LEGACY_URL` (7.x); without those env vars the
//! testcontainers fallback launches the appropriate image.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::MongoObjectIdValue;
use air_elt_commons_testing::mongo::{mongo_pool, mongo_pool_legacy};
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};
use bson::doc;
use bson::oid::ObjectId;

async fn round_trip_overwrite(sink: MongoSink, db: &str, client: &mongodb::Client) {
    let spec = WriteSpec {
        columns: vec!["_id".into(), "label".into()],
        table: "items".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // First batch — pure inserts via upsert.
    let batch_a = Batch {
        rows: (1_i64..=3)
            .map(|i| Row::upsert(vec![Value::Int64(i), Value::Text(format!("v1-{i}"))]))
            .collect(),
        next_cursor: None,
    };
    let report_a = sink
        .write_batch(&spec, ctx.clone(), &batch_a)
        .await
        .unwrap();
    assert_eq!(report_a.rows_written, 3, "first batch: 3 upserts");

    // Second batch — same _ids, new labels — exercises the matched/replace
    // half of the upsert path.
    let batch_b = Batch {
        rows: (1_i64..=3)
            .map(|i| Row::upsert(vec![Value::Int64(i), Value::Text(format!("v2-{i}"))]))
            .collect(),
        next_cursor: None,
    };
    let report_b = sink.write_batch(&spec, ctx, &batch_b).await.unwrap();
    assert_eq!(report_b.rows_written, 3, "second batch: 3 replacements");

    let coll = client.database(db).collection::<bson::Document>("items");
    let two = coll.find_one(doc! { "_id": 2_i64 }).await.unwrap().unwrap();
    assert_eq!(two.get_str("label").unwrap(), "v2-2");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_write_path_on_modern_server() {
    let handle = mongo_pool().await;
    let sink = MongoSink::connect(MongoSinkConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");
    let v = sink.server_version();
    assert!(
        v.supports_bulk_write(),
        "AIR_ELT_TEST_MONGO_URL must point at a >=8.0 server, got {}.{}",
        v.major,
        v.minor
    );
    round_trip_overwrite(sink, &handle.database, &handle.client).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_path_compound_key_on_legacy_server() {
    let handle = mongo_pool_legacy().await;
    let sink = MongoSink::connect(MongoSinkConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");
    assert!(!sink.server_version().supports_bulk_write());
    round_trip_overwrite_compound_key(sink, &handle.database, &handle.client).await;
}

/// Drives the legacy `update` run_command path through a compound
/// (non-`_id`) conflict key — the path that bypasses the `_id`
/// fast-path and exercises `build_upsert_filter`'s nested-field
/// branch. Unit tests cover the filter shape; this test proves the
/// real server actually matches+replaces against it.
async fn round_trip_overwrite_compound_key(sink: MongoSink, db: &str, client: &mongodb::Client) {
    let spec = WriteSpec {
        columns: vec!["tenant".into(), "addr.city".into(), "label".into()],
        table: "compound_items".into(),
        conflict: Some(ConflictConfig {
            key: vec!["tenant".into(), "addr.city".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch_a = Batch {
        rows: vec![
            Row::upsert(vec![
                Value::Text("acme".into()),
                Value::Text("Berlin".into()),
                Value::Text("v1-a".into()),
            ]),
            Row::upsert(vec![
                Value::Text("acme".into()),
                Value::Text("Paris".into()),
                Value::Text("v1-b".into()),
            ]),
        ],
        next_cursor: None,
    };
    let report_a = sink
        .write_batch(&spec, ctx.clone(), &batch_a)
        .await
        .unwrap();
    assert_eq!(report_a.rows_written, 2, "two upserts via compound key");

    // Same compound keys, new label — must replace, not duplicate.
    let batch_b = Batch {
        rows: vec![Row::upsert(vec![
            Value::Text("acme".into()),
            Value::Text("Berlin".into()),
            Value::Text("v2-a".into()),
        ])],
        next_cursor: None,
    };
    let report_b = sink.write_batch(&spec, ctx, &batch_b).await.unwrap();
    assert_eq!(report_b.rows_written, 1, "one matched replacement");

    let coll = client
        .database(db)
        .collection::<bson::Document>("compound_items");
    let count = coll
        .count_documents(doc! { "tenant": "acme" })
        .await
        .unwrap();
    assert_eq!(count, 2, "no duplicates after overwrite");
    let berlin = coll
        .find_one(doc! { "tenant": "acme", "addr.city": "Berlin" })
        .await
        .unwrap()
        .unwrap();
    assert_eq!(berlin.get_str("label").unwrap(), "v2-a");
}

/// `_id: ObjectId` mapping written via the modern `bulk_write` path
/// must land as `Bson::ObjectId` on the server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bulk_write_object_id_lands_as_bson_object_id() {
    let handle = mongo_pool().await;
    let sink = MongoSink::connect(MongoSinkConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");
    assert!(sink.server_version().supports_bulk_write());

    let spec = WriteSpec {
        columns: vec!["_id".into(), "label".into()],
        table: "bulk_oid".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let oids: Vec<ObjectId> = (0..3).map(|_| ObjectId::new()).collect();
    let batch = Batch {
        rows: oids
            .iter()
            .enumerate()
            .map(|(i, o)| {
                Row::upsert(vec![
                    Value::Custom(Box::new(MongoObjectIdValue(o.bytes()))),
                    Value::Text(format!("v-{i}")),
                ])
            })
            .collect(),
        next_cursor: None,
    };
    let report = sink.write_batch(&spec, ctx, &batch).await.unwrap();
    assert_eq!(report.rows_written, 3);

    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("bulk_oid");
    for o in &oids {
        let doc_one = coll
            .find_one(doc! { "_id": *o })
            .await
            .expect("find")
            .expect("doc");
        match doc_one.get("_id").expect("present") {
            bson::Bson::ObjectId(read_oid) => assert_eq!(read_oid, o),
            other => panic!("expected Bson::ObjectId, got {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_path_on_legacy_server() {
    let handle = mongo_pool_legacy().await;
    let sink = MongoSink::connect(MongoSinkConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");
    let v = sink.server_version();
    assert!(
        !v.supports_bulk_write(),
        "AIR_ELT_TEST_MONGO_LEGACY_URL must point at a <8.0 server, got {}.{}",
        v.major,
        v.minor
    );
    round_trip_overwrite(sink, &handle.database, &handle.client).await;
}
