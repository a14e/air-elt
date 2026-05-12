//! Basic e2e: connect to ClickHouse, create a sandbox table, INSERT
//! one row via the sink, read it back. Also verifies that the sink
//! drops `RowOp::Delete` rows silently — they never reach CH.

use std::sync::Arc;

use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_core::config::conflict::ConflictConfig;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_clickhouse::{ChSink, ChSinkConfig};

// No silent skip: `clickhouse_handle()` either resolves an external CH
// (via `AIR_ELT_TEST_CLICKHOUSE_URL`) or starts a testcontainer (via
// the auto-detected docker/podman socket). If neither is available the
// handle panics with a clear message — matching the project convention
// that flag-gated test skips are not allowed.

#[tokio::test]
async fn round_trip_simple_columns() {
    let h = clickhouse_handle().await;
    h.exec(
        "CREATE TABLE users (id UInt64, name String, age Nullable(Int32)) ENGINE = MergeTree() \
         ORDER BY id",
    )
    .await
    .expect("create table");
    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    assert!(!sink.supports_deletes(), "ChSink must declare no-delete");

    let spec = WriteSpec {
        table: "users".to_string(),
        columns: vec!["id".into(), "name".into(), "age".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![
            Row {
                values: vec![
                    Value::UInt64(1),
                    Value::Text("alice".into()),
                    Value::Int32(30),
                ],
                op: RowOp::Upsert,
            },
            // This delete should NOT reach the sink in production (the
            // runner pre-filters). We submit it directly to exercise the
            // defensive sink-side filter inside `write_batch`.
            Row {
                values: vec![Value::UInt64(2), Value::Text("ignored".into()), Value::Null],
                op: RowOp::Delete,
            },
        ],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, Arc::clone(&ctx), batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written, 1, "delete must be filtered");

    let body = h
        .exec("SELECT count() FROM users FORMAT TabSeparated")
        .await
        .expect("select");
    assert_eq!(body.trim(), "1");
}

#[tokio::test]
async fn validate_delete_access_is_default_ok_but_runner_skips_it() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE t (id UInt64) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");
    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "t".to_string(),
        columns: vec!["id".into()],
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: air_elt_core::config::conflict::ConflictStrategy::Overwrite,
        }),
    };
    // Default impl returns Ok(()) even though CH can't actually do
    // mutations. The validation pipeline gates this call on
    // `sink.supports_deletes()` so it's never invoked in practice.
    sink.validate_delete_access(&spec)
        .await
        .expect("default ok");
}
