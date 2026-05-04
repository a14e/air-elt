//! Versioning e2e for the mongo-sink upsert path.
//!
//! The sink picks one of two implementations at `connect()` based on
//! the connected server version:
//!   - **8.0+**: `Client::bulk_write` (single round-trip per batch).
//!   - **<8.0**: `replace_one` loop with bounded parallelism.
//!
//! These tests assert the version is detected correctly *and* that
//! the upsert round-trip works end-to-end on each path. Connection
//! URLs come from `AIR_ELT_TEST_MONGO_URL` (8.0) and
//! `AIR_ELT_TEST_MONGO_LEGACY_URL` (7.x); without those env vars the
//! testcontainers fallback launches the appropriate image.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mongo::{mongo_pool, mongo_pool_legacy};
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};
use bson::doc;

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
