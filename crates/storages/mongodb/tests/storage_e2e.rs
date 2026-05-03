#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Storage;
use air_elt_core::types::Value;
use air_elt_storage_mongodb::{MongoStorage, MongoStorageConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn save_and_load_cursor_roundtrip() {
    let handle = mongo_pool().await;
    let storage = MongoStorage::connect(MongoStorageConfig {
        url: handle.url.clone(),
        database: Some(handle.database.clone()),
        ..Default::default()
    })
    .await
    .expect("connect");

    storage.validate_access().await.expect("validate_access");
    storage.migrate().await.expect("migrate");

    assert!(storage.load_cursor("flow_x").await.unwrap().is_none());

    let state = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(42),
    }]);
    storage.save_cursor("flow_x", &state).await.unwrap();

    let loaded = storage
        .load_cursor("flow_x")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(loaded, state);

    let state2 = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(99),
    }]);
    storage.save_cursor("flow_x", &state2).await.unwrap();
    let loaded2 = storage
        .load_cursor("flow_x")
        .await
        .unwrap()
        .expect("present");
    assert_eq!(loaded2.fields[0].value, Value::Int64(99));
}
