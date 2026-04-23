#![allow(clippy::unwrap_used)]
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
