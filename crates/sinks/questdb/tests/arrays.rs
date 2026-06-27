//! Native QuestDB `DOUBLE[]` array write e2e (AIR-124).
//!
//! QuestDB >= 9.0 stores 1-D arrays of `DOUBLE` and speaks them over
//! pg-wire as binary `_float8` (OID 1022) — the exact wire shape the sink
//! binds via `pg_bind::bind_double_array` (`Vec<f64>`). This is the first
//! live round-trip of that bind path against a real server; the in-crate
//! unit tests only assert the bind chain, not server acceptance.
//!
//! Covers the shapes that distinguish the array bind from a scalar:
//!   * a populated `DOUBLE[]`,
//!   * an empty array,
//!   * a whole-column NULL array (the typed-NULL array path).
#![allow(clippy::unwrap_used)]

use chrono::{TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn double_array_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("arr_double").await;
    h.exec(
        "CREATE TABLE arr_double ( \
            ts   TIMESTAMP, \
            vals DOUBLE[] \
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .expect("create table");

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "arr_double".to_string(),
        columns: vec!["ts".into(), "vals".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 3, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let rows = vec![
        Row::upsert(vec![
            Value::Timestamp(base),
            Value::Array(vec![
                Value::Float64(1.0),
                Value::Float64(2.5),
                Value::Float64(-3.0),
            ]),
        ]),
        Row::upsert(vec![
            Value::Timestamp(base + chrono::Duration::seconds(1)),
            Value::Array(vec![]),
        ]),
        Row::upsert(vec![
            Value::Timestamp(base + chrono::Duration::seconds(2)),
            Value::Null,
        ]),
    ];

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
    assert_eq!(report.rows_written(), 3);

    // QuestDB WAL-apply is asynchronous — poll for the expected count.
    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM arr_double")
            .fetch_one(&h.pool)
            .await
            .expect("count query");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, 3);

    let read = sqlx::query("SELECT vals FROM arr_double ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select");
    assert_eq!(read.len(), 3);

    // Row 0 — populated array round-trips element-for-element.
    let populated = read[0].try_get::<Vec<f64>, _>("vals").expect("vals row 0");
    assert_eq!(populated, vec![1.0, 2.5, -3.0]);

    // Row 1 — empty array stays an (empty) array, not NULL.
    let empty = read[1].try_get::<Vec<f64>, _>("vals").expect("vals row 1");
    assert!(empty.is_empty(), "empty DOUBLE[] must round-trip as empty");

    // Row 2 — whole-column NULL.
    let null_col = read[2].try_get::<Option<Vec<f64>>, _>("vals").unwrap();
    assert_eq!(
        null_col, None,
        "NULL DOUBLE[] column must read back as None"
    );

    h.drop_table("arr_double").await;
    h.pool.close().await;
}
