//! Storage e2e against **MariaDB**. The mysql-protocol driver is shared with
//! stock MySQL, so this is a focused smoke test rather than a full mirror —
//! it exists to exercise the version-aware UPSERT fallback (`pick_upsert_cursor`
//! routes MariaDB to the legacy `VALUES()` form, since MariaDB never adopted
//! the row-alias syntax MySQL 8.0.19+ uses).
#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mariadb::{MariaDbTestHandle, mariadb_pool};
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Storage;
use air_elt_core::types::Value;
use air_elt_storage_mysql::{MySqlStorage, MySqlStorageConfig};

async fn make_storage(handle: &MariaDbTestHandle) -> MySqlStorage {
    MySqlStorage::connect(MySqlStorageConfig {
        url: handle.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("connect storage")
}

#[tokio::test]
async fn migrate_and_upsert_cursor_on_mariadb() {
    let handle = mariadb_pool().await;
    let storage = make_storage(&handle).await;

    storage.migrate().await.expect("migrate");
    let flow = "flow_mariadb";

    // Two saves to the same flow: the second must overwrite via the legacy
    // `ON DUPLICATE KEY UPDATE state = VALUES(state)` path.
    let s1 = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(1),
    }]);
    storage.save_cursor(flow, &s1).await.expect("save 1");

    let s2 = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(2),
    }]);
    storage
        .save_cursor(flow, &s2)
        .await
        .expect("save 2 (upsert)");

    let loaded = storage
        .load_cursor(flow)
        .await
        .expect("load")
        .expect("present");
    assert_eq!(loaded.fields[0].value, Value::Int64(2));

    handle.pool.close().await;
}
