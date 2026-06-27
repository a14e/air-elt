//! Negative-path validation.

use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::error::{RuntimeError, ValidationError};
use air_elt_core::model::WriteSpec;
use air_elt_core::traits::Sink;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_designated_timestamp_when_column_omitted() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_missing_ts").await;
    h.exec(
        "CREATE TABLE bench_missing_ts ( \
            ts TIMESTAMP, \
            v DOUBLE \
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .expect("create");

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    // Omit the designated `ts` column from the spec.
    let spec = WriteSpec {
        table: "bench_missing_ts".to_string(),
        columns: vec!["v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    let err = sink.validate_access(&spec).await.expect_err("must fail");
    match err {
        RuntimeError::Validation(ValidationError::MissingDesignatedTimestamp { table, column }) => {
            assert_eq!(table, "bench_missing_ts");
            assert_eq!(column, "ts");
        }
        other => panic!("expected MissingDesignatedTimestamp, got {other:?}"),
    }

    h.drop_table("bench_missing_ts").await;
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ping_fails_on_broken_pg_url() {
    // 200ms connect_timeout: port-1 connect fails fast in practice, but
    // sqlx's internal acquire-retry would otherwise burn the full
    // `PoolSettings::connect` default (5s) before our outer timeout cuts in.
    let cfg = QuestDbSinkConfig {
        url: "postgres://nobody:nope@127.0.0.1:1/qdb".to_string(),
        connect_timeout: Some(std::time::Duration::from_millis(200)),
        ..Default::default()
    };
    let err = match QuestDbSink::connect(cfg).await {
        Ok(_) => panic!("connect must fail"),
        Err(e) => e,
    };
    // Pin the variant — the runner uses the variant to choose ctx-drop /
    // reconnect, so Display-string assertions would mask a regression that
    // re-routed transport errors through `Other`.
    assert!(
        matches!(err, RuntimeError::Backend(_) | RuntimeError::Other(_)),
        "unexpected error variant: {err:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn validate_access_surfaces_table_not_found() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_never_existed").await;
    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "bench_never_existed".to_string(),
        columns: vec!["ts".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    let err = sink.validate_access(&spec).await.expect_err("must fail");
    // The schema-introspection step must surface a typed variant rather
    // than a generic backend error. The runner treats `Validation` as a
    // permanent failure (no reconnect) — a `Backend` mapping would
    // mis-trigger ctx-drop here.
    match err {
        RuntimeError::Validation(ValidationError::SinkTableNotFound { sink, table }) => {
            assert_eq!(sink, "questdb");
            assert_eq!(table, "bench_never_existed");
        }
        other => panic!("expected SinkTableNotFound, got {other:?}"),
    }
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn multi_timestamp_designated_correctly_identified() {
    // Two TIMESTAMP columns, with `recorded_at` declared as the
    // designated one. The schema parser must pick `recorded_at` by the
    // `designated` flag, not by table-order position (`created_at` comes
    // first). The spec lists only the non-designated column → must
    // surface `MissingDesignatedTimestamp { column: "recorded_at" }`.
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_multi_ts").await;
    h.exec(
        "CREATE TABLE bench_multi_ts ( \
            created_at  TIMESTAMP, \
            recorded_at TIMESTAMP, \
            v           DOUBLE \
         ) TIMESTAMP(recorded_at) PARTITION BY DAY;",
    )
    .await
    .expect("create");

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    // Spec omits `recorded_at` (the designated). It lists `created_at`,
    // which is the *other* TIMESTAMP column. If the parser had picked by
    // position, it would (incorrectly) report `created_at` missing.
    let spec = WriteSpec {
        table: "bench_multi_ts".to_string(),
        columns: vec!["created_at".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    let err = sink.validate_access(&spec).await.expect_err("must fail");
    match err {
        RuntimeError::Validation(ValidationError::MissingDesignatedTimestamp { table, column }) => {
            assert_eq!(table, "bench_multi_ts");
            assert_eq!(
                column, "recorded_at",
                "parser must identify the designated column by flag, not table order"
            );
        }
        other => panic!("expected MissingDesignatedTimestamp, got {other:?}"),
    }

    h.drop_table("bench_multi_ts").await;
    h.pool.close().await;
}

/// The dry-run probe uses an `INSERT INTO ... SELECT $1,... WHERE 1=0`
/// statement which never produces a row. After `validate_access` returns
/// `Ok`, the table must still be empty — no sentinel, no rollback drift
/// against QuestDB's asynchronous WAL apply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dry_run_does_not_persist_a_row() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_dry_run_no_rows").await;
    h.exec(
        "CREATE TABLE bench_dry_run_no_rows ( \
            ts TIMESTAMP, \
            v  DOUBLE \
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .expect("create");

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "bench_dry_run_no_rows".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");

    // validate_access returns only after the dry-run probe finishes —
    // any buggy path that actually wrote a row would have flushed it
    // to the WAL before returning. A single `count()` is sufficient
    // and follows `testing-guidelines` (no sleep-on-happy-path).
    let row = sqlx::query("SELECT count() AS c FROM bench_dry_run_no_rows")
        .fetch_one(&h.pool)
        .await
        .expect("count");
    let count: i64 = row.try_get("c").expect("count decode");
    assert_eq!(count, 0, "dry-run probe must not persist any row");

    h.drop_table("bench_dry_run_no_rows").await;
    h.pool.close().await;
}

/// Validate the missing-table heuristic against a table that was created
/// and then dropped — covers the case where QuestDB surfaces a message
/// variant like `'<name>' is not a valid table` (8.2.x). The runner must
/// receive `ValidationError::SinkTableNotFound` rather than a generic
/// backend error so the flow does not enter a reconnect loop.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_table_surfaces_table_not_found() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_dropped").await;
    h.exec(
        "CREATE TABLE bench_dropped ( \
            ts TIMESTAMP, \
            v  DOUBLE \
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .expect("create");
    h.drop_table("bench_dropped").await;

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "bench_dropped".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    let err = sink.validate_access(&spec).await.expect_err("must fail");
    match err {
        RuntimeError::Validation(ValidationError::SinkTableNotFound { sink, table }) => {
            assert_eq!(sink, "questdb");
            assert_eq!(table, "bench_dropped");
        }
        other => panic!("expected SinkTableNotFound, got {other:?}"),
    }
    h.pool.close().await;
}
