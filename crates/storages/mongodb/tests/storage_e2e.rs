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

    handle.client.clone().shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_token_round_trip_and_reopen() {
    let handle = mongo_pool().await;
    let make = || async {
        MongoStorage::connect(MongoStorageConfig {
            url: handle.url.clone(),
            database: Some(handle.database.clone()),
            ..Default::default()
        })
        .await
        .expect("connect")
    };
    let storage = make().await;
    storage.migrate().await.expect("migrate");

    assert!(storage.load_resume_token("flow_a").await.unwrap().is_none());

    let token = serde_json::json!({ "_data": "82AABB" });
    storage.save_resume_token("flow_a", &token).await.unwrap();
    assert_eq!(
        storage.load_resume_token("flow_a").await.unwrap().unwrap(),
        token
    );

    let token2 = serde_json::json!({ "_data": "82CCDD" });
    storage.save_resume_token("flow_a", &token2).await.unwrap();
    assert_eq!(
        storage.load_resume_token("flow_a").await.unwrap().unwrap(),
        token2
    );

    // Reopen through a fresh client — the dedicated
    // `air_elt_resume_tokens` collection must persist across handles.
    drop(storage);
    let storage2 = make().await;
    assert_eq!(
        storage2.load_resume_token("flow_a").await.unwrap().unwrap(),
        token2
    );
    assert!(
        storage2
            .load_resume_token("flow_b")
            .await
            .unwrap()
            .is_none()
    );

    handle.client.clone().shutdown().await;
}
