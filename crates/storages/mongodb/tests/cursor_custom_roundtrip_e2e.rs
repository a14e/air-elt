//! Round-trip a cursor containing `Value::Custom(MongoObjectIdValue)`
//! through the mongo storage. This is the load-bearing case for the
//! cursor-Custom path because Mongo is the only backend that emits
//! ObjectId cursors in production. The storage encodes the value via
//! `Value::Serialize` (kind + `to_json`), then
//! `load_cursor(flow, &[DataType::Custom(MongoObjectIdType)])` recovers
//! the same `Value::Custom` through `DataType::decode_cursor_json` —
//! without a global registry.
#![allow(clippy::unwrap_used)]

use air_elt_commons_mongodb::types::{MongoObjectIdType, MongoObjectIdValue};
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Storage;
use air_elt_core::types::{DataType, Value};
use air_elt_storage_mongodb::{MongoStorage, MongoStorageConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mongo_storage_round_trips_objectid_cursor_value() {
    let handle = mongo_pool().await;
    let storage = MongoStorage::connect(
        MongoStorageConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        },
        std::sync::Arc::new(air_elt_commons_mongodb::MongoPoolStatsReader::new()),
    )
    .await
    .expect("connect");
    storage.migrate().await.expect("migrate");

    let flow = "flow_oid_cursor";
    let oid_bytes: [u8; 12] = [
        0x65, 0x4f, 0x10, 0x80, 0x01, 0x02, 0x03, 0x04, 0x05, 0x00, 0x00, 0x01,
    ];
    let state = CursorState::new(vec![CursorFieldValue {
        name: "_id".into(),
        value: Value::Custom(Box::new(MongoObjectIdValue(oid_bytes))),
    }]);

    storage
        .save_cursor(flow, &state, false)
        .await
        .expect("save_cursor");

    let cursor_types = [DataType::Custom(Box::new(MongoObjectIdType))];
    let loaded = storage
        .load_cursor(flow, &cursor_types)
        .await
        .expect("load_cursor")
        .expect("present");
    assert_eq!(loaded, state, "ObjectId cursor must round-trip identically");
}
