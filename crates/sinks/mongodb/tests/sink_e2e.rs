#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};
use bson::doc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn insert_and_upsert_with_dot_notation() {
    let handle = mongo_pool().await;
    let sink = MongoSink::connect(MongoSinkConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");

    // Upsert on `_id` so re-writing the same id replaces the existing
    // document instead of erroring with E11000. Exercises the `_id`
    // fast path in `MongoSink::write_batch`.
    let spec = WriteSpec {
        columns: vec!["_id".into(), "name".into(), "addr.city".into()],
        table: "users".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![
            Row {
                values: vec![
                    Value::Int64(1),
                    Value::Text("alice".into()),
                    Value::Text("Berlin".into()),
                ],
            },
            Row {
                values: vec![
                    Value::Int64(2),
                    Value::Text("bob".into()),
                    Value::Text("Munich".into()),
                ],
            },
        ],
        next_cursor: None,
    };
    let report = sink.write_batch(&spec, ctx.clone(), &batch).await.unwrap();
    assert_eq!(report.rows_written, 2);

    // Re-write same id 1 with new city — upsert path replaces it.
    let batch2 = Batch {
        rows: vec![Row {
            values: vec![
                Value::Int64(1),
                Value::Text("alice".into()),
                Value::Text("Hamburg".into()),
            ],
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, ctx, &batch2).await.unwrap();

    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("users");
    let doc_alice = coll
        .find_one(doc! { "_id": 1_i64 })
        .await
        .expect("find")
        .expect("doc");
    let inner = doc_alice.get_document("addr").unwrap();
    assert_eq!(inner.get_str("city").unwrap(), "Hamburg");
}
