//! `_id: ObjectId` exercised as the cursor field — verifies that the
//! `MongoObjectIdType::can_be_cursor() = true` flag is honoured end-to-end
//! and that ObjectId values flow through the cursor pagination loop
//! without ever being flattened to `Bytes(12)`.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::MongoObjectIdValue;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::Value;
use air_elt_source_mongodb::{MongoSource, MongoSourceConfig};
use bson::doc;
use bson::oid::ObjectId;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_objectid_paginates() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("cursor_oid");
    // ObjectIds generated in close succession keep monotonic order via
    // the embedded counter — good enough for the pagination invariant.
    let mut oids: Vec<ObjectId> = Vec::with_capacity(3);
    for i in 0..3 {
        let o = ObjectId::new();
        coll.insert_one(doc! { "_id": o, "n": i as i64 })
            .await
            .expect("seed");
        oids.push(o);
    }

    let source = MongoSource::connect(
        "test_source_cursor_oid".into(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["_id".into(), "n".into()],
        table: "cursor_oid".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 2,
        source_options: toml::Table::new(),
    };
    let ctx = source.build_context(&spec).await.expect("ctx");
    let batch1 = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("batch1");
    assert_eq!(batch1.rows.len(), 2);
    let cursor = batch1.next_cursor.expect("cursor");
    assert_eq!(cursor.fields.len(), 1);
    assert_eq!(cursor.fields[0].name, "_id");
    match &cursor.fields[0].value {
        Value::Custom(v) => {
            let oid = v
                .as_any()
                .downcast_ref::<MongoObjectIdValue>()
                .expect("downcast cursor MongoObjectIdValue");
            assert_eq!(oid.0, oids[1].bytes());
        }
        other => panic!("expected Value::Custom on cursor, got {other:?}"),
    }

    let batch2 = source
        .read_batch(&spec, ctx, Some(&cursor))
        .await
        .expect("batch2");
    assert_eq!(batch2.rows.len(), 1);
}
