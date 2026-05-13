//! E2E for MS SQL storage: migrate idempotency, cursor + resume token
//! round-trip, dry_run leaves no open transaction.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mssql::mssql_pool;
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Storage;
use air_elt_core::types::Value;
use air_elt_storage_mssql::{MssqlStorage, MssqlStorageConfig};

async fn make_storage(url: String) -> MssqlStorage {
    MssqlStorage::connect(MssqlStorageConfig {
        url,
        ..Default::default()
    })
    .await
    .expect("connect storage")
}

/// The user-mandated idempotency check: applying `migrate()` twice on a
/// fresh database must succeed both times AND leave the ledger with exactly
/// one row per known migration version.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrate_is_idempotent() {
    let ms = mssql_pool().await;
    let storage = make_storage(ms.url_with_database()).await;

    storage.migrate().await.expect("first migrate");
    storage
        .migrate()
        .await
        .expect("second migrate (idempotent)");
    // Third time for good measure.
    storage.migrate().await.expect("third migrate (idempotent)");

    // Ledger must contain exactly one row per known version.
    let mut conn = ms.pool.get().await.unwrap();
    let stream = conn
        .simple_query(&format!(
            "SELECT version FROM [{}].dbo._air_elt_migrations ORDER BY version",
            ms.database
        ))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    let versions: Vec<i32> = rows
        .iter()
        .map(|r| r.try_get::<i32, _>(0).unwrap().unwrap())
        .collect();
    assert_eq!(
        versions,
        vec![0, 1, 2],
        "ledger must record exactly one row per migration including the zero bootstrap"
    );

    // Tables exist exactly once.
    let stream = conn
        .simple_query(&format!(
            "SELECT name FROM [{}].sys.tables WHERE name IN ('air_elt_cursors','air_elt_resume_tokens','_air_elt_migrations') ORDER BY name",
            ms.database
        ))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    let names: Vec<String> = rows
        .iter()
        .map(|r| r.try_get::<&str, _>(0).unwrap().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        vec![
            "_air_elt_migrations".to_string(),
            "air_elt_cursors".to_string(),
            "air_elt_resume_tokens".to_string()
        ],
        "all three storage tables must exist exactly once"
    );
    drop(conn);
    drop(ms);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cursor_save_load_roundtrip() {
    let ms = mssql_pool().await;
    let storage = make_storage(ms.url_with_database()).await;
    storage.migrate().await.expect("migrate");

    let flow = "flow_one";
    assert!(storage.load_cursor(flow).await.unwrap().is_none());

    let state = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(42),
    }]);
    storage.save_cursor(flow, &state, false).await.unwrap();
    assert_eq!(storage.load_cursor(flow).await.unwrap().unwrap(), state);

    // Overwrite (MERGE).
    let state2 = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(100),
    }]);
    storage.save_cursor(flow, &state2, false).await.unwrap();
    assert_eq!(
        storage.load_cursor(flow).await.unwrap().unwrap().fields[0].value,
        Value::Int64(100)
    );
    drop(ms);
}

/// dry_run path must wrap the write in BEGIN TRY / ROLLBACK / CATCH so the
/// connection's `@@TRANCOUNT` returns to 0 — no orphan transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_save_cursor_leaves_no_open_transaction() {
    let ms = mssql_pool().await;
    let storage = make_storage(ms.url_with_database()).await;
    storage.migrate().await.expect("migrate");

    let state = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(7),
    }]);
    storage
        .save_cursor("dry_flow", &state, true)
        .await
        .expect("dry_run save_cursor");
    // No row written.
    assert!(
        storage.load_cursor("dry_flow").await.unwrap().is_none(),
        "dry_run must not persist anything"
    );

    // A fresh connection must observe @@TRANCOUNT = 0.
    let mut conn = ms.pool.get().await.unwrap();
    let stream = conn.simple_query("SELECT @@TRANCOUNT").await.unwrap();
    let rows = stream.into_first_result().await.unwrap();
    let trancount: i32 = rows[0].try_get::<i32, _>(0).unwrap().unwrap();
    assert_eq!(trancount, 0, "no orphan transaction after dry_run");
    drop(conn);
    drop(ms);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_token_roundtrip() {
    let ms = mssql_pool().await;
    let storage = make_storage(ms.url_with_database()).await;
    storage.migrate().await.expect("migrate");

    let flow = "rs_flow";
    assert!(storage.load_resume_token(flow).await.unwrap().is_none());

    let token = serde_json::json!({"_data": "abc123"});
    storage
        .save_resume_token(flow, &token, false)
        .await
        .unwrap();
    assert_eq!(
        storage.load_resume_token(flow).await.unwrap().unwrap(),
        token
    );
    drop(ms);
}
