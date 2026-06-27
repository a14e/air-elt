//! JavaScript-code round-trip: source reads `Bson::JavaScriptCode`,
//! sink writes it back as `Bson::JavaScriptCode` (not as a plain string).
//!
//! Confirms `MongoJsType` survives the source → sink hop end-to-end on
//! a mongo→mongo pipeline.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::MongoJsValue;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, ReadSpec, Row, WriteSpec};
use air_elt_core::traits::{Sink, Source};
use air_elt_core::types::Value;
use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};
use air_elt_source_mongodb::{MongoSource, MongoSourceConfig};
use bson::doc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn javascript_round_trips_through_mongo_to_mongo() {
    let handle = mongo_pool().await;
    let src_coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("js_source");
    let code = "function () { return 1; }".to_string();
    src_coll
        .insert_one(doc! { "_id": 1_i64, "code": bson::Bson::JavaScriptCode(code.clone()) })
        .await
        .expect("seed");

    let source = MongoSource::connect(
        "test_js_source".into(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect source");

    let read_spec = ReadSpec {
        columns: vec!["_id".into(), "code".into()],
        table: "js_source".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };
    let ctx = source.build_context(&read_spec).await.expect("ctx");
    let batch = source
        .read_batch(&read_spec, &ctx, None)
        .await
        .expect("read");
    assert_eq!(batch.rows.len(), 1);
    match &batch.rows[0].values[1] {
        Value::Custom(v) => {
            let inner = v
                .as_any()
                .downcast_ref::<MongoJsValue>()
                .expect("downcast MongoJsValue");
            assert_eq!(inner.0, code);
        }
        other => panic!("expected Value::Custom(MongoJsValue), got {other:?}"),
    }

    let sink = MongoSink::connect(
        MongoSinkConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect sink");
    let write_spec = WriteSpec {
        columns: vec!["_id".into(), "code".into()],
        table: "js_target".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&write_spec)
        .await
        .expect("validate sink");
    let sink_ctx = sink
        .build_context(&write_spec)
        .await
        .expect("sink build_context");

    let written = Batch {
        rows: vec![Row::upsert(batch.rows[0].values.clone())],
        next_cursor: None,
    };
    sink.write_batch(&write_spec, &sink_ctx, written, false)
        .await
        .expect("write");

    let target = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("js_target");
    let doc_one = target
        .find_one(doc! { "_id": 1_i64 })
        .await
        .expect("find")
        .expect("doc");
    match doc_one.get("code").expect("present") {
        bson::Bson::JavaScriptCode(s) => assert_eq!(s, &code),
        other => panic!("expected Bson::JavaScriptCode, got {other:?}"),
    }
}
