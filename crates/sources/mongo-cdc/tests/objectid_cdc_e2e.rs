//! Change-stream CDC e2e: insert/update a document with an
//! `ObjectId`-typed `_id`. The source must emit
//! `Value::Custom(MongoObjectIdValue)` for that column rather than the
//! legacy flattened `Bytes(12)` shape.
//!
//! Uses the same probe-watch + post-batch resume token (PBRT) pattern
//! as the main `e2e.rs` to avoid races on change-stream visibility.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use air_elt_commons_mongodb::bson_value;
use air_elt_commons_mongodb::types::MongoObjectIdValue;
use air_elt_commons_testing::mongo::{MongoTestHandle, mongo_rs_pool};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::model::{CursorFieldValue, CursorState, ReadSpec, RowOp};
use air_elt_core::traits::Source;
use air_elt_core::types::Value;
use air_elt_source_mongo_cdc::{MongoCdcSource, MongoCdcSourceConfig};
use bson::oid::ObjectId;
use bson::{Document, doc};
use mongodb::Collection;

const RESUME_TOKEN_FIELD: &str = "__resume_token";

async fn cdc_source(handle: &MongoTestHandle) -> Arc<MongoCdcSource> {
    let cfg = MongoCdcSourceConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        max_await_time: Some(Duration::from_millis(200)),
        ..Default::default()
    };
    let s = MongoCdcSource::connect(
        "mongo_cdc_oid".into(),
        cfg,
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect mongo-cdc");
    Arc::new(s)
}

fn cdc_spec(table: &str, limit: usize) -> ReadSpec {
    let mut opts = toml::Table::new();
    opts.insert("mode".into(), toml::Value::String("post-image".into()));
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
async fn cdc_emits_object_id_as_custom_value() {
    let mongo = mongo_rs_pool().await;
    let coll_name = "oid_cdc";
    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<Document>(coll_name);
    let seed_oid = ObjectId::new();
    coll.insert_one(doc! { "_id": seed_oid, "name": "seed" })
        .await
        .expect("seed");
    enable_pre_post_images(&mongo, coll_name).await;

    let source = cdc_source(&mongo).await;
    let spec = cdc_spec(coll_name, 4);
    let ctx = source.build_context(&spec).await.expect("ctx");

    let cursor = capture_pbrt(&coll).await;
    let oid_a = ObjectId::new();
    let oid_b = ObjectId::new();
    // Four events so the read_batch fast-path
    // (`events.len() >= spec.limit`) trips. Dedup-by-`_id` collapses
    // them to two rows: final post-image for oid_a and oid_b. The
    // Custom-typed _id assertion below is unchanged.
    coll.insert_one(doc! { "_id": oid_a, "name": "alice" })
        .await
        .unwrap();
    coll.insert_one(doc! { "_id": oid_b, "name": "bob" })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": oid_a }, doc! { "$set": { "name": "alice2" } })
        .await
        .unwrap();
    coll.update_one(doc! { "_id": oid_b }, doc! { "$set": { "name": "bob2" } })
        .await
        .unwrap();

    let batch = source
        .read_batch(&spec, &ctx, Some(&cursor))
        .await
        .expect("read");
    // Dedup by _id collapses insert(a)+update(a) → final post-image of a.
    assert_eq!(batch.rows.len(), 2);
    let mut bytes_seen: Vec<[u8; 12]> = Vec::new();
    for row in &batch.rows {
        assert_eq!(row.op, RowOp::Upsert);
        match &row.values[0] {
            Value::Custom(v) => {
                let oid = v
                    .as_any()
                    .downcast_ref::<MongoObjectIdValue>()
                    .expect("downcast MongoObjectIdValue");
                bytes_seen.push(oid.0);
            }
            other => panic!("expected Value::Custom(MongoObjectIdValue), got {other:?}"),
        }
    }
    assert!(bytes_seen.contains(&oid_a.bytes()));
    assert!(bytes_seen.contains(&oid_b.bytes()));
}
