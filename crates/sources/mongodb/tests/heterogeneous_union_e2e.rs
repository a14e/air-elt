//! Heterogeneous source field (`int + string` in the same column).
//! Sample-based inference must emit `DataType::Union(...)` and
//! `read_batch` must decode each row into the matching `Value` variant.
//! Lived in `crates/app/tests/mongo_to_mongo.rs` previously, but the
//! invariants here are entirely about the mongo source — moving the
//! coverage here lets the cross-vendor app tests stay focused on
//! pipeline glue.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_mongodb::{MongoSource, MongoSourceConfig};
use bson::doc;
use bson::oid::ObjectId;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heterogeneous_field_inferred_as_union_and_read_back() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("things");
    coll.insert_many(vec![
        doc! { "_id": 1_i64, "value": 42_i32 },
        doc! { "_id": 2_i64, "value": "hello" },
        doc! { "_id": 3_i64, "value": 99_i32 },
    ])
    .await
    .unwrap();

    let source = MongoSource::connect(
        "test_union".to_string(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let schema = source.describe_schema("things").await.expect("describe");
    let value_field = schema.find("value").expect("value field");
    match &value_field.data_type {
        DataType::Union(vs) => {
            assert!(
                vs.contains(&DataType::Int32),
                "Union must include Int32 from int observations: {vs:?}"
            );
            assert!(
                vs.contains(&DataType::Text { size: None }),
                "Union must include unbounded Text from string observations: {vs:?}"
            );
        }
        other => panic!("expected Union for heterogeneous column, got {other:?}"),
    }

    let spec = ReadSpec {
        columns: vec!["_id".into(), "value".into()],
        table: "things".into(),
        cursor_fields: vec!["_id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };
    source.validate_access(&spec).await.expect("validate");
    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, ctx, None)
        .await
        .expect("read_batch");

    assert_eq!(batch.rows.len(), 3);
    // The actual `Value` variant per row mirrors the source BSON type —
    // Union dispatch happens later, in `convert`, not at read time.
    assert_eq!(batch.rows[0].values[1], Value::Int32(42));
    assert_eq!(batch.rows[1].values[1], Value::Text("hello".into()));
    assert_eq!(batch.rows[2].values[1], Value::Int32(99));
}

/// Heterogeneous field with an ObjectId observation alongside a string:
/// inferred type must become `Union(Custom(MongoObjectIdType), Text)`,
/// confirming Custom-types participate in Union schemas correctly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heterogeneous_field_with_object_id_emits_union_with_custom() {
    let handle = mongo_pool().await;
    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("oid_union");
    let oid = ObjectId::new();
    coll.insert_many(vec![
        doc! { "_id": 1_i64, "value": oid },
        doc! { "_id": 2_i64, "value": "stringy" },
    ])
    .await
    .unwrap();

    let source = MongoSource::connect(
        "test_oid_union".into(),
        MongoSourceConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let schema = source.describe_schema("oid_union").await.expect("describe");
    let f = schema.find("value").expect("value field");
    match &f.data_type {
        DataType::Union(vs) => {
            assert_eq!(vs.len(), 2);
            assert!(
                vs.iter().any(|v| matches!(
                    v,
                    DataType::Custom(t) if t.kind() == air_elt_commons_mongodb::types::MongoObjectIdType::KIND
                )),
                "Union must include ObjectId Custom: {vs:?}"
            );
            assert!(
                vs.contains(&DataType::Text { size: None }),
                "Union must include unbounded Text: {vs:?}"
            );
        }
        other => panic!("expected Union, got {other:?}"),
    }
}
