//! Dry-run path for `MongoSink::write_batch` (T6).
//!
//! `dry_run = true` must build the same docs as production and ship
//! them via `replaceOne(filter={$expr:false}, doc, upsert=false)` —
//! the server parses each BSON document but never matches, so the
//! collection stays empty and `WriteReport::rows_written` reads zero.

#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::BsonObjectValue;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::{ConversionContext, ConvertError, DataType, DynType, DynValue, Value};
use air_elt_sink_mongodb::{MongoSink, MongoSinkConfig};
use bson::doc;
use std::any::Any;

/// Per-field projection path: 2 docs through the `_id` upsert plan.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_batch_dry_run_skips_writes_then_real_run_commits() {
    let handle = mongo_pool().await;
    let sink = MongoSink::connect(
        MongoSinkConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect");

    let spec = WriteSpec {
        columns: vec!["_id".into(), "name".into()],
        table: "dry_run_users".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
        sink_options: Default::default(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let make_batch = || Batch {
        rows: vec![
            Row::upsert(vec![Value::Int64(1), Value::Text("alice".into())]),
            Row::upsert(vec![Value::Int64(2), Value::Text("bob".into())]),
        ],
        next_cursor: None,
    };

    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("dry_run_users");

    let report_dry = sink
        .write_batch(&spec, &ctx, make_batch(), true)
        .await
        .expect("dry-run write");
    assert_eq!(report_dry.rows_written(), 0);
    let count_after_dry = coll.count_documents(doc! {}).await.unwrap();
    assert_eq!(
        count_after_dry, 0,
        "dry-run must leave the target collection empty"
    );

    let report = sink
        .write_batch(&spec, &ctx, make_batch(), false)
        .await
        .expect("real write");
    assert_eq!(report.rows_written(), 2);
    let count_after = coll.count_documents(doc! {}).await.unwrap();
    assert_eq!(count_after, 2);
}

/// Raw-passthrough fast-path under dry-run: schemaless-both `["*"]`
/// lowers to a single `_root` body target. Rows hold one
/// `Value::Custom(BsonObjectValue)`; `build_docs` recognises the shape
/// and ships the documents via the never-matching `replaceOne` filter,
/// leaving the collection empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn raw_passthrough_dry_run_skips_writes() {
    use air_elt_core::mapping::ROOT_BODY_TARGET;
    use air_elt_core::model::RowOp;

    let handle = mongo_pool().await;
    let sink = MongoSink::connect(
        MongoSinkConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect");

    let spec = WriteSpec {
        columns: vec![ROOT_BODY_TARGET.to_string()],
        table: "dry_run_raw".into(),
        conflict: None,
        sink_options: Default::default(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let d1 = doc! { "_id": 1_i64, "name": "alice" };
    let d2 = doc! { "_id": 2_i64, "name": "bob" };
    let batch = Batch {
        rows: vec![
            Row {
                values: vec![Value::Custom(Box::new(BsonObjectValue(d1.clone())))],
                body: None,
                op: RowOp::Upsert,
            },
            Row {
                values: vec![Value::Custom(Box::new(BsonObjectValue(d2.clone())))],
                body: None,
                op: RowOp::Upsert,
            },
        ],
        next_cursor: None,
    };

    let report = sink.write_batch(&spec, &ctx, batch, true).await.unwrap();
    assert_eq!(report.rows_written(), 0);

    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("dry_run_raw");
    let count = coll.count_documents(doc! {}).await.unwrap();
    assert_eq!(
        count, 0,
        "raw-passthrough dry-run must not commit any documents"
    );
}

/// Stub Custom type / value with no mongo encoder mapping. Used to
/// prove that the dry-run delete path now exercises
/// `bson_value::to_bson_owned` per row — encoding such a value must
/// return `Err`, surfacing wire-encoding bugs that the previous
/// single-`$expr:false` filter would have masked.
#[derive(Debug)]
struct StubType;

impl DynType for StubType {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn kind(&self) -> &str {
        "test.unknown_custom"
    }
    fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
        false
    }
    fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
        false
    }
    fn convert(
        &self,
        _v: Value,
        _t: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        unreachable!()
    }
    fn construct(
        &self,
        _v: Value,
        _t: &DataType,
        _ctx: &ConversionContext,
    ) -> Result<Value, ConvertError> {
        unreachable!()
    }
    fn clone_box(&self) -> Box<dyn DynType> {
        Box::new(StubType)
    }
}

#[derive(Debug)]
struct StubValue;

impl DynValue for StubValue {
    fn dyn_type(&self) -> Box<dyn DynType> {
        Box::new(StubType)
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
    fn is_equal(&self, _other: &dyn DynValue) -> bool {
        false
    }
    fn clone_box(&self) -> Box<dyn DynValue> {
        Box::new(StubValue)
    }
}

/// Dry-run delete must build the production filter shape and run every
/// per-row key value through `bson_value::to_bson_owned`. Earlier
/// versions short-circuited with a single `{ $expr: false }` filter,
/// silently hiding wire-encoding bugs in the delete-key path.
///
/// Two assertions:
///  1. With a valid key the dry-run delete leaves the collection
///     untouched (target documents survive); the subsequent real
///     delete removes them — proves the dry-run is non-destructive.
///  2. With a deliberately broken key (`Value::Custom` of an
///     unsupported kind) the dry-run delete returns `Err`, proving the
///     per-row encoding path is now exercised.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_delete_validates_per_row_key_encoding() {
    let handle = mongo_pool().await;
    let sink = MongoSink::connect(
        MongoSinkConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect");

    let spec = WriteSpec {
        columns: vec!["_id".into(), "name".into()],
        table: "dry_run_delete_keys".into(),
        conflict: Some(ConflictConfig {
            key: vec!["_id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
        sink_options: Default::default(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Seed two documents we'd like to "delete" in dry-run.
    let seed = Batch {
        rows: vec![
            Row::upsert(vec![Value::Int64(1), Value::Text("alice".into())]),
            Row::upsert(vec![Value::Int64(2), Value::Text("bob".into())]),
        ],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, seed, false)
        .await
        .expect("seed");

    let coll = handle
        .client
        .database(&handle.database)
        .collection::<bson::Document>("dry_run_delete_keys");
    assert_eq!(coll.count_documents(doc! {}).await.unwrap(), 2);

    // (1) Valid keys → dry-run delete must NOT mutate the collection.
    let dry_delete = Batch {
        rows: vec![
            Row::delete(vec![Value::Int64(1), Value::Null]),
            Row::delete(vec![Value::Int64(2), Value::Null]),
        ],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, dry_delete, true)
        .await
        .expect("dry-run delete");
    assert_eq!(report.rows_written(), 0);
    assert_eq!(
        coll.count_documents(doc! {}).await.unwrap(),
        2,
        "dry-run delete must leave the collection untouched"
    );

    // (2) Broken key (unsupported `Value::Custom`) → dry-run delete
    // must return Err because per-row keys now flow through
    // `to_bson_owned`. Without the fix, the path issued a single
    // `$expr:false` filter and silently swallowed encoding errors.
    let broken = Batch {
        rows: vec![Row::delete(vec![
            Value::Custom(Box::new(StubValue)),
            Value::Null,
        ])],
        next_cursor: None,
    };
    let err = sink
        .write_batch(&spec, &ctx, broken, true)
        .await
        .expect_err("dry-run delete with unencodable key must error");
    let msg = format!("{err}");
    assert!(
        msg.contains("unsupported Value::Custom") || msg.contains("test.unknown_custom"),
        "unexpected error: {msg}"
    );

    // Real delete still works against the seeded docs, confirming the
    // production path was unaffected by the refactor.
    let real_delete = Batch {
        rows: vec![
            Row::delete(vec![Value::Int64(1), Value::Null]),
            Row::delete(vec![Value::Int64(2), Value::Null]),
        ],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, real_delete, false)
        .await
        .expect("real delete");
    assert_eq!(report.rows_written(), 2);
    assert_eq!(coll.count_documents(doc! {}).await.unwrap(), 0);
}
