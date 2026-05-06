#![allow(clippy::unwrap_used)]
use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::{CursorFieldValue, CursorState};
use air_elt_core::traits::Storage;
use air_elt_core::types::Value;
use air_elt_storage_postgres::{PgStorage, PgStorageConfig};
use chrono::{NaiveDate, TimeZone, Utc};
use uuid::Uuid;

/// Return both the handle and a storage bound to its sandbox schema. The
/// caller must keep the handle alive for the duration of the test — dropping
/// it early would tear down the schema and leave the storage pointing at
/// nothing, causing a pool timeout on the next request.
async fn make_storage(handle: &air_elt_commons_testing::pg::PgTestHandle) -> PgStorage {
    PgStorage::connect(PgStorageConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect storage")
}

#[tokio::test]
async fn migrate_and_upsert_cursor() {
    let handle = pg_pool().await;
    let storage = PgStorage::connect(PgStorageConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect storage");

    storage
        .validate_access()
        .await
        .expect("validate_access pre-migrate");
    storage.migrate().await.expect("migrate");
    storage
        .validate_access()
        .await
        .expect("validate_access post-migrate");

    let flow = "flow_one";
    assert!(storage.load_cursor(flow).await.unwrap().is_none());

    let state = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(42),
    }]);
    storage
        .save_cursor(flow, &state)
        .await
        .expect("save_cursor");
    assert_eq!(storage.load_cursor(flow).await.unwrap().unwrap(), state);

    let state2 = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(43),
    }]);
    storage.save_cursor(flow, &state2).await.expect("upsert");
    assert_eq!(
        storage.load_cursor(flow).await.unwrap().unwrap().fields[0].value,
        Value::Int64(43)
    );
}

/// Exercise the tagged-serde round-trip for every `Value` variant through
/// JSONB storage. If anyone swaps `#[serde(tag = "type", content = "value")]`
/// for untagged, this test catches the `Int64(42) → Int16(42)` coercion bug
/// immediately.
#[tokio::test]
async fn cursor_roundtrip_all_value_variants() {
    let handle = pg_pool().await;
    let storage = make_storage(&handle).await;
    storage.migrate().await.expect("migrate");

    let date = NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
    let ts = Utc.with_ymd_and_hms(2026, 4, 22, 12, 0, 0).unwrap();
    let uuid = Uuid::from_u128(0x0123456789abcdef_0123456789abcdef);
    let json = serde_json::json!({"nested": {"x": 1}, "arr": [1, 2, 3]});

    let variants: Vec<(&str, Value)> = vec![
        ("c_null", Value::Null),
        ("c_bool", Value::Bool(true)),
        ("c_i16", Value::Int16(-42)),
        ("c_i32", Value::Int32(-7_000_000)),
        ("c_i64", Value::Int64(9_000_000_000_000)),
        ("c_f32", Value::Float32(1.25)),
        ("c_f64", Value::Float64(-1.234567890123456)),
        ("c_text", Value::Text("привет ☕".into())),
        ("c_bytes", Value::Bytes(vec![0xff, 0x00, 0xab])),
        ("c_date", Value::Date(date)),
        ("c_ts", Value::Timestamp(ts)),
        ("c_uuid", Value::Uuid(uuid)),
        ("c_json", Value::Json(json.clone())),
        // edge cases: empty collections
        ("c_bytes_empty", Value::Bytes(vec![])),
        ("c_text_empty", Value::Text(String::new())),
        ("c_json_empty_obj", Value::Json(serde_json::json!({}))),
        // BigInt and Decimal serialize through their string-form custom serde
        // shims (default JSON number repr would f64-truncate). Test values
        // above f64 mantissa to lock the contract in.
        (
            "c_bigint",
            Value::BigInt(
                num_bigint::BigInt::parse_bytes(b"12345678901234567890123456", 10).unwrap(),
            ),
        ),
        (
            "c_decimal",
            Value::Decimal("1234567890.0987654321".parse().unwrap()),
        ),
    ];

    for (flow, value) in &variants {
        let state = CursorState::new(vec![CursorFieldValue {
            name: "v".into(),
            value: value.clone(),
        }]);
        storage
            .save_cursor(flow, &state)
            .await
            .unwrap_or_else(|e| panic!("save {flow}: {e}"));
        let loaded = storage
            .load_cursor(flow)
            .await
            .unwrap_or_else(|e| panic!("load {flow}: {e}"))
            .unwrap_or_else(|| panic!("missing cursor for {flow}"));
        assert_eq!(loaded, state, "{flow}: variant did not round-trip");
    }
}

#[tokio::test]
async fn resume_token_round_trip_and_reopen() {
    let handle = pg_pool().await;
    let storage = make_storage(&handle).await;
    storage.migrate().await.expect("migrate");

    assert!(
        storage.load_resume_token("flow_a").await.unwrap().is_none(),
        "unknown flow → None"
    );

    let token = serde_json::json!({ "_data": "82AABB", "extra": [1, 2] });
    storage
        .save_resume_token("flow_a", &token)
        .await
        .expect("save");
    let loaded = storage
        .load_resume_token("flow_a")
        .await
        .expect("load")
        .expect("present");
    assert_eq!(loaded, token);

    // Upsert path: save again with a different value.
    let token2 = serde_json::json!({ "_data": "82CCDD" });
    storage.save_resume_token("flow_a", &token2).await.unwrap();
    assert_eq!(
        storage.load_resume_token("flow_a").await.unwrap().unwrap(),
        token2
    );

    // Persistence-across-reopen: dropping the storage handle must not
    // lose the row.
    drop(storage);
    let storage2 = make_storage(&handle).await;
    assert_eq!(
        storage2.load_resume_token("flow_a").await.unwrap().unwrap(),
        token2
    );
    assert!(
        storage2
            .load_resume_token("flow_b")
            .await
            .unwrap()
            .is_none(),
        "unknown flow after reopen → None"
    );
}

#[tokio::test]
async fn cockroach_migrate_and_save_load_round_trip() {
    let handle = cockroach_pool().await;
    let storage = PgStorage::connect(PgStorageConfig {
        dialect: Dialect::Cockroach,
        url: handle.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("connect cockroach storage");

    storage.migrate().await.expect("cockroach migrate");

    let flow = "flow_x";
    assert!(storage.load_cursor(flow).await.unwrap().is_none());

    let state = CursorState::new(vec![CursorFieldValue {
        name: "id".into(),
        value: Value::Int64(101),
    }]);
    storage
        .save_cursor(flow, &state)
        .await
        .expect("cockroach save_cursor");
    assert_eq!(storage.load_cursor(flow).await.unwrap().unwrap(), state);

    let token = serde_json::json!({ "_data": "82AABB" });
    storage
        .save_resume_token(flow, &token)
        .await
        .expect("cockroach save_resume_token");
    assert_eq!(
        storage.load_resume_token(flow).await.unwrap().unwrap(),
        token
    );
}

#[tokio::test]
async fn cockroach_migrate_idempotent() {
    let handle = cockroach_pool().await;
    let storage = PgStorage::connect(PgStorageConfig {
        dialect: Dialect::Cockroach,
        url: handle.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("connect cockroach storage");

    storage.migrate().await.expect("cockroach migrate first");
    storage.migrate().await.expect("cockroach migrate second");
}
