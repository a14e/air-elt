//! pg-wire bind-param chunking at the `QDB_PG_MAX_BIND_PARAMS = 9_200`
//! cap. A 2_000-row × 5-column batch produces 10_000 binds which must
//! split into at least two statements (chunk size = 1_840 rows per
//! statement).

use chrono::{Duration, TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_wire_chunks_large_batch_at_bind_param_cap() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_pg_chunking").await;
    h.exec(
        "CREATE TABLE bench_pg_chunking ( \
            ts TIMESTAMP, \
            a LONG, \
            b LONG, \
            c LONG, \
            d LONG \
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
        table: "bench_pg_chunking".to_string(),
        columns: vec!["ts".into(), "a".into(), "b".into(), "c".into(), "d".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Build 2_000 rows. 2_000 × 5 = 10_000 binds → at the 9_200 cap the
    // writer must chunk into at least 2 statements.
    let base = Utc
        .with_ymd_and_hms(2025, 9, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let mut rows = Vec::with_capacity(2_000);
    for i in 0_i64..2_000 {
        rows.push(Row::upsert(vec![
            // Unique microsecond per row so QuestDB doesn't dedup on `ts`.
            Value::Timestamp(base + Duration::microseconds(i)),
            Value::Int64(i),
            Value::Int64(i * 2),
            Value::Int64(i * 3),
            Value::Int64(i * 4),
        ]));
    }
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows,
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written, 2_000);

    // WAL-apply is asynchronous — poll for the expected count up to 10s.
    let mut row_count: i64 = 0;
    for _ in 0..100 {
        let row = sqlx::query("SELECT count() AS n FROM bench_pg_chunking")
            .fetch_one(&h.pool)
            .await
            .expect("count");
        row_count = row.try_get::<i64, _>("n").expect("count decode");
        if row_count == 2_000 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        row_count, 2_000,
        "expected 2_000 rows after pg-chunked write"
    );

    h.drop_table("bench_pg_chunking").await;
    h.pool.close().await;
}
