//! Mongo sink raw-passthrough fast-path.
//!
//! Schemaless-both `["*"]` (mongo→mongo) lowers to a Transform with a
//! single `Body` op writing the synthetic `_root` target. The
//! sink receives a regular `Batch` whose rows carry one
//! `Value::Custom(BsonObjectValue(doc))`; `build_docs` recognises the
//! shape and emits the document verbatim.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::BsonObjectValue;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::mapping::ROOT_BODY_TARGET;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};
use bson::doc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_passthrough_writes_documents_verbatim() {
    let handle = mongo_pool().await;
    let sink = MongoSink::connect(MongoSinkConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");

    // Schemaless-both `["*"]` lowering: WriteSpec carries one synthetic
    // `_root` column; rows hold a single `Value::Custom(BsonObjectValue)`.
    // The sink recognises the shape and writes the document at root.
    let spec = WriteSpec {
        columns: vec![ROOT_BODY_TARGET.to_string()],
        table: "raw_sink".into(),
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let d1 = doc! { "_id": 1_i64, "name": "alice", "addr": { "city": "Berlin" } };
    let d2 = doc! { "_id": 2_i64, "name": "bob", "tags": ["x", "y"] };
    let batch = Batch {
        rows: vec![
            Row {
                values: vec![Value::Custom(Box::new(BsonObjectValue(d1.clone())))],
                op: RowOp::Upsert,
            },
            Row {
                values: vec![Value::Custom(Box::new(BsonObjectValue(d2.clone())))],
                op: RowOp::Upsert,
            },
        ],
        next_cursor: None,
    };
    let report = sink.write_batch(&spec, &ctx, batch, false).await.unwrap();
    assert_eq!(report.rows_written, 2);

    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("raw_sink");
    let read1 = coll
        .find_one(doc! { "_id": 1_i64 })
        .await
        .expect("find")
        .expect("doc1");
    let read2 = coll
        .find_one(doc! { "_id": 2_i64 })
        .await
        .expect("find")
        .expect("doc2");
    assert_eq!(read1, d1);
    assert_eq!(read2, d2);
}
