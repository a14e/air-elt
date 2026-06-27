//! Corner-case sweep — defensive coverage for behaviours that are easy to
//! regress but boring to catch on a happy-path test.

use chrono::{TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::error::{ConfigError, RuntimeError};
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

/// All-delete batch on a no-delete sink. Under the post-AIR-70
/// contract the runner ships the FULL batch (deletes included) to a
/// `supports_deletes() == false` sink; the sink is the authoritative
/// filter and must drop the deletes, report `rows_written = 0`, and
/// not touch the writer at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_delete_batch_writes_zero_rows() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_all_delete").await;
    h.exec(
        "CREATE TABLE bench_all_delete ( \
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
        table: "bench_all_delete".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2025, 11, 1, 0, 0, 0)
        .single()
        .expect("ts");
    // Two synthetic delete rows. The sink reports zero writes and emits
    // no traffic.
    let delete_row = Row {
        values: vec![Value::Timestamp(ts), Value::Float64(1.0)],
        body: None,
        op: RowOp::Delete,
    };
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows: vec![delete_row.clone(), delete_row],
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 0);
    assert_eq!(
        report.rows_skipped(),
        2,
        "both Delete rows must be surfaced via `skipped` so the runner can \
         increment `air_elt_rows_total{{stage=skipped, op=delete}}`"
    );

    h.drop_table("bench_all_delete").await;
    h.pool.close().await;
}

/// Empty batch — sink reports zero writes and never opens a writer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_batch_writes_zero_rows() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_empty_batch").await;
    h.exec(
        "CREATE TABLE bench_empty_batch ( \
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
        table: "bench_empty_batch".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows: vec![],
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 0);

    h.drop_table("bench_empty_batch").await;
    h.pool.close().await;
}

/// `[flow.x.conflict]` carried by `WriteSpec` is rejected at the head
/// of `validate_access`, before any transport probe.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn conflict_rejected() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_conflict_pg").await;
    h.exec(
        "CREATE TABLE bench_conflict_pg ( \
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
        table: "bench_conflict_pg".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: Some(ConflictConfig {
            key: vec!["ts".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
        sink_options: toml::Table::new(),
    };
    let err = sink.validate_access(&spec).await.expect_err("must fail");
    match err {
        RuntimeError::Config(ConfigError::ConflictNotSupported { sink, .. }) => {
            assert_eq!(sink, "questdb");
        }
        other => panic!("expected ConflictNotSupported, got {other:?}"),
    }

    h.drop_table("bench_conflict_pg").await;
    h.pool.close().await;
}

/// 30-column wide row exercises bind-position swizzling in the pg-wire
/// plan. If column ordering misaligned the binds, the spot-checked
/// column would land in the wrong slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wide_row_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_wide_row").await;
    // 30 columns: 1 ts + 14 longs + 14 doubles + 1 string.
    let mut ddl = String::from("CREATE TABLE bench_wide_row ( ts TIMESTAMP");
    for i in 0..14 {
        ddl.push_str(&format!(", i{i} LONG"));
    }
    for i in 0..14 {
        ddl.push_str(&format!(", f{i} DOUBLE"));
    }
    ddl.push_str(", s STRING ) TIMESTAMP(ts) PARTITION BY DAY;");
    h.exec(&ddl).await.expect("create");

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    let mut columns: Vec<String> = vec!["ts".into()];
    for i in 0..14 {
        columns.push(format!("i{i}"));
    }
    for i in 0..14 {
        columns.push(format!("f{i}"));
    }
    columns.push("s".into());
    assert_eq!(columns.len(), 30);
    let spec = WriteSpec {
        table: "bench_wide_row".to_string(),
        columns: columns.clone(),
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2025, 10, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let mut values: Vec<Value> = vec![Value::Timestamp(ts)];
    for i in 0..14_i64 {
        values.push(Value::Int64(1000 + i));
    }
    for i in 0..14 {
        values.push(Value::Float64(0.5 + i as f64));
    }
    values.push(Value::Text("wide".into()));
    let row = Row::upsert(values);
    sink.write_batch(
        &spec,
        &ctx,
        Batch {
            rows: vec![row],
            next_cursor: None,
        },
        false,
    )
    .await
    .expect("write");

    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_wide_row")
            .fetch_one(&h.pool)
            .await
            .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, 1);

    // Spot-check three columns: i0, f7, s. Catches bind-position bugs
    // — if the plan emitted i0 at the wrong slot, this query would
    // surface a mismatch.
    let row = sqlx::query("SELECT i0, f7, s FROM bench_wide_row")
        .fetch_one(&h.pool)
        .await
        .expect("spot check");
    let i0: i64 = row.try_get("i0").expect("i0");
    let f7: f64 = row.try_get("f7").expect("f7");
    let s: String = row.try_get("s").expect("s");
    assert_eq!(i0, 1000);
    assert!((f7 - 7.5).abs() < 1e-9);
    assert_eq!(s, "wide");

    h.drop_table("bench_wide_row").await;
    h.pool.close().await;
}

/// One-row batch on the pg-wire path. Trivial happy path, but verifies
/// the chunking loop handles a single chunk correctly (off-by-one
/// regression at chunk size = 1).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_single_row_chunk() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_pg_single").await;
    h.exec(
        "CREATE TABLE bench_pg_single ( \
            ts TIMESTAMP, \
            payload BINARY \
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
        table: "bench_pg_single".to_string(),
        columns: vec!["ts".into(), "payload".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2025, 12, 2, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![Value::Timestamp(ts), Value::Bytes(vec![0xff, 0xaa])]);
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows: vec![row],
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write");
    assert_eq!(report.rows_written(), 1);

    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_pg_single")
            .fetch_one(&h.pool)
            .await
            .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, 1);

    h.drop_table("bench_pg_single").await;
    h.pool.close().await;
}

/// All-NULL row (besides the designated timestamp). Exercises typed-NULL
/// binding for every non-designated column. QuestDB rejects a literal
/// NULL value at the designated-timestamp column at DDL level, so the
/// designated slot still carries a real `Timestamp`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_null_row_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_all_null_row").await;
    h.exec(
        "CREATE TABLE bench_all_null_row ( \
            ts TIMESTAMP, \
            a  LONG, \
            b  DOUBLE, \
            c  STRING, \
            d  BINARY \
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
        table: "bench_all_null_row".to_string(),
        columns: vec!["ts".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![
        Value::Timestamp(ts),
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
    ]);
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows: vec![row],
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write");
    assert_eq!(report.rows_written(), 1);

    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query(
            "SELECT count() AS c FROM bench_all_null_row \
             WHERE a IS NULL AND b IS NULL AND c IS NULL AND d IS NULL",
        )
        .fetch_one(&h.pool)
        .await
        .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, 1);

    h.drop_table("bench_all_null_row").await;
    h.pool.close().await;
}
