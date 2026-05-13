//! Mongo source raw-passthrough mode.
//!
//! When `ReadSpec.columns` is empty (the wildcard expansion
//! signal for schemaless-both flows), `read_batch` emits one row per
//! document carrying the whole document on `RawRow.body` as a
//! `BsonObjectValue` — no per-field projection, no `next_cursor`.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::{BsonObjectType, BsonObjectValue};
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_source_mongodb::{MongoSource, MongoSourceConfig};
use bson::doc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_batch_raw_mode_emits_bson_object_rows() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("raw_src");
    let d1 = doc! { "_id": 1_i64, "name": "alice", "addr": { "city": "Berlin" } };
    let d2 = doc! { "_id": 2_i64, "name": "bob", "tags": ["x", "y"] };
    coll.insert_one(d1.clone()).await.expect("seed1");
    coll.insert_one(d2.clone()).await.expect("seed2");

    let source = MongoSource::connect(
        "raw_src".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    // Empty `columns` + empty `cursor_fields` — the raw
    // passthrough shape from `expand`'s wildcard branch.
    let spec = ReadSpec {
        columns: vec![],
        table: "raw_src".into(),
        cursor_fields: vec![],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, &ctx, None)
        .await
        .expect("read_batch");

    assert_eq!(batch.rows.len(), 2);
    assert!(
        batch.next_cursor.is_none(),
        "raw passthrough must not emit a cursor"
    );

    // Source advertises schemaless.
    assert!(<MongoSource as Source>::schemaless(&source));

    // Each row is a passthrough RawRow: empty `values`, payload on `body`.
    for row in &batch.rows {
        assert!(row.values.is_empty(), "passthrough rows have no values");
        let v = row.body.clone().expect("passthrough row has body");
        match v {
            air_elt_core::types::Value::Custom(dv) => {
                let any = dv.into_any();
                let bo = any
                    .downcast::<BsonObjectValue>()
                    .expect("BsonObjectValue payload");
                let id = bo.0.get_i64("_id").unwrap();
                assert!(matches!(id, 1 | 2));
            }
            other => panic!("expected Value::Custom(BsonObjectValue), got {other:?}"),
        }
    }
    // Silence unused-import lint when the BsonObject* helpers are
    // covered by other tests in this file.
    let _ = (BsonObjectType::KIND, std::mem::size_of::<BsonObjectValue>());
}

/// Body-flow read: per-column `ReadSpec.columns` populated, plus
/// `needs_body: true` (set when the expanded mapping has body
/// targets). Each emitted row carries both the per-column values AND
/// the source `bson::Document` on `RawRow.body` as a `BsonObjectValue`
/// — verified via downcast.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_batch_attaches_body_when_needs_body_set() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("body_src");
    let d1 = doc! { "_id": 10_i64, "name": "alice", "extra": 7_i32 };
    let d2 = doc! { "_id": 20_i64, "name": "bob", "extra": 9_i32 };
    coll.insert_one(d1.clone()).await.expect("seed1");
    coll.insert_one(d2.clone()).await.expect("seed2");

    let source = MongoSource::connect(
        "body_src".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["_id".into(), "name".into()],
        table: "body_src".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: true,
    };

    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, &ctx, None)
        .await
        .expect("read_batch");

    assert_eq!(batch.rows.len(), 2);
    for row in &batch.rows {
        assert_eq!(row.values.len(), 2, "per-column values still populated");
        let v = row
            .body
            .clone()
            .expect("needs_body=true must attach RawRow.body");
        match v {
            air_elt_core::types::Value::Custom(dv) => {
                let any = dv.into_any();
                let bo = any
                    .downcast::<BsonObjectValue>()
                    .expect("BsonObjectValue payload");
                let id = bo.0.get_i64("_id").unwrap();
                let extra = bo.0.get_i32("extra").unwrap();
                assert!(matches!(id, 10 | 20));
                assert!(matches!(extra, 7 | 9));
            }
            other => panic!("expected Value::Custom(BsonObjectValue), got {other:?}"),
        }
    }
}

/// Cost-guard regression at the source layer: with `needs_body=false`
/// (the default for non-body flows) `RawRow.body` is `None` for every
/// row — no document clone, no allocation past the per-column values
/// vec.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_batch_skips_body_when_needs_body_unset() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("body_src_off");
    let d1 = doc! { "_id": 1_i64, "name": "alice" };
    coll.insert_one(d1).await.expect("seed");

    let source = MongoSource::connect(
        "body_src_off".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["_id".into(), "name".into()],
        table: "body_src_off".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, &ctx, None)
        .await
        .expect("read_batch");

    assert_eq!(batch.rows.len(), 1);
    assert!(
        batch.rows[0].body.is_none(),
        "needs_body=false must leave RawRow.body=None (cost guard)"
    );
}
