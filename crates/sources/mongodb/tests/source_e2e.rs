//! End-to-end test for the MongoDB source connector.
//!
//! Honours `AIR_ELT_TEST_MONGO_URL` for CI; otherwise spins up a
//! mongo:7 testcontainer.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::Value;
use air_elt_source_mongodb::{MongoSource, MongoSourceConfig};
use bson::doc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_with_cursor_and_dot_notation() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("users");
    for i in 1_i64..=5 {
        coll.insert_one(doc! {
            "_id": i,
            "name": format!("user-{i}"),
            "addr": { "city": "Berlin" },
        })
        .await
        .expect("seed");
    }

    let source = MongoSource::connect(
        "test_source".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["_id".into(), "name".into(), "addr.city".into()],
        table: "users".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 3,
        source_options: toml::Table::new(),
    };

    source
        .validate_access(&spec)
        .await
        .expect("validate_access");
    let ctx = source.build_context(&spec).await.expect("build_context");

    let batch = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("read_batch first");
    assert_eq!(batch.rows.len(), 3);
    assert_eq!(batch.rows[0].values[0], Value::Int64(1));
    assert_eq!(batch.rows[0].values[2], Value::Text("Berlin".into()));

    let next = batch.next_cursor.expect("next cursor");
    let batch2 = source
        .read_batch(&spec, ctx, Some(&next))
        .await
        .expect("read_batch second");
    assert_eq!(batch2.rows.len(), 2);
    assert_eq!(batch2.rows[0].values[0], Value::Int64(4));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sample_returns_documents() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("metrics");
    for i in 0_i64..20 {
        coll.insert_one(doc! { "_id": i, "v": i * 2 })
            .await
            .expect("seed");
    }

    let source = MongoSource::connect(
        "test_source".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["_id".into(), "v".into()],
        table: "metrics".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 1024,
        source_options: toml::Table::new(),
    };

    let rows = source.sample(&spec, 5).await.expect("sample");
    assert!(!rows.is_empty());
    assert!(rows.len() <= 5);
    for row in &rows {
        assert_eq!(row.values.len(), 2);
    }
}

/// Compound `(updated_at, _id)` cursor — the typical idempotent ELT
/// shape. Two rows share the same `updated_at`, forcing the source to
/// fall back to the secondary key (`_id`) for tiebreaking. The first
/// batch must stop on the boundary; the second batch must pick up the
/// other tied row plus the next-`updated_at` rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compound_cursor_updated_at_id() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("events");
    // Seeded shape: updated_at = 100 has two rows (_id=1,2); 200 has
    // one (_id=3); 300 has one (_id=4). Total 4. With limit=2 we
    // expect: batch1 = (100,1)+(100,2); batch2 = (200,3)+(300,4).
    for (ua, id) in [(100_i64, 1_i64), (100, 2), (200, 3), (300, 4)] {
        coll.insert_one(doc! { "_id": id, "updated_at": ua, "label": format!("e{id}") })
            .await
            .expect("seed");
    }

    let source = MongoSource::connect(
        "test_source".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["_id".into(), "updated_at".into(), "label".into()],
        table: "events".into(),
        cursor_fields: vec!["updated_at".into(), "_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 2,
        source_options: toml::Table::new(),
    };

    let ctx = source.build_context(&spec).await.expect("build_context");

    let batch1 = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("batch1");
    assert_eq!(batch1.rows.len(), 2, "batch1 should be the two ua=100 rows");
    assert_eq!(batch1.rows[0].values[0], Value::Int64(1));
    assert_eq!(batch1.rows[1].values[0], Value::Int64(2));
    let cursor1 = batch1.next_cursor.expect("cursor1");
    assert_eq!(cursor1.fields.len(), 2);
    assert_eq!(cursor1.fields[0].name, "updated_at");
    assert_eq!(cursor1.fields[0].value, Value::Int64(100));
    assert_eq!(cursor1.fields[1].name, "_id");
    assert_eq!(cursor1.fields[1].value, Value::Int64(2));

    let batch2 = source
        .read_batch(&spec, ctx.clone(), Some(&cursor1))
        .await
        .expect("batch2");
    assert_eq!(batch2.rows.len(), 2, "batch2 should pick up ua=200 and 300");
    assert_eq!(batch2.rows[0].values[0], Value::Int64(3));
    assert_eq!(batch2.rows[1].values[0], Value::Int64(4));

    let cursor2 = batch2.next_cursor.expect("cursor2");
    let batch3 = source
        .read_batch(&spec, ctx, Some(&cursor2))
        .await
        .expect("batch3 (drain)");
    assert_eq!(batch3.rows.len(), 0, "no more rows after the last id");
}
