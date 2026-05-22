//! Basic round-trip with a typical time-series schema (5 rows, pg-wire).

use chrono::{TimeZone, Utc};
use sqlx::Row as _;
use uuid::Uuid;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn basic_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_basic").await;
    h.exec(
        "CREATE TABLE bench_basic ( \
            ts TIMESTAMP, \
            device_id LONG, \
            temperature DOUBLE, \
            active BOOLEAN, \
            uid UUID \
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .expect("create table");

    let cfg = QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    };
    let sink = QuestDbSink::connect(cfg).await.expect("connect");
    assert!(!sink.supports_deletes());

    let spec = WriteSpec {
        table: "bench_basic".to_string(),
        columns: vec![
            "ts".into(),
            "device_id".into(),
            "temperature".into(),
            "active".into(),
            "uid".into(),
        ],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base_ts = Utc
        .with_ymd_and_hms(2025, 1, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let mut rows = Vec::new();
    for i in 0..5_i64 {
        rows.push(Row::upsert(vec![
            Value::Timestamp(base_ts + chrono::Duration::seconds(i)),
            Value::Int64(100 + i),
            Value::Float64(20.0 + i as f64),
            Value::Bool(i % 2 == 0),
            // Mix `Uuid::max()` and `Uuid::nil()` plus a v4 to catch any
            // hypothetical swap-nibble / all-zeros aliasing.
            Value::Uuid(match i % 3 {
                0 => Uuid::max(),
                1 => Uuid::nil(),
                _ => Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").expect("uuid"),
            }),
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
    assert_eq!(report.rows_written(), 5);

    // QuestDB WAL-apply is asynchronous — poll for the expected count
    // up to 5s before giving up.
    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_basic")
            .fetch_one(&h.pool)
            .await
            .expect("count query");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == 5 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, 5);

    h.drop_table("bench_basic").await;
    h.pool.close().await;
}

/// UUID round-trip with explicit boundary values: `Uuid::max()` (all-FF)
/// and a random-v4. Catches swap-nibble bugs that would pass if every
/// test value were the all-zero nil UUID.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uuid_boundary_values_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_uuid_boundary").await;
    h.exec(
        "CREATE TABLE bench_uuid_boundary ( \
            ts TIMESTAMP, \
            uid UUID \
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
        table: "bench_uuid_boundary".to_string(),
        columns: vec!["ts".into(), "uid".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 1, 2, 0, 0, 0)
        .single()
        .expect("ts");
    let v4 = Uuid::parse_str("123e4567-e89b-12d3-a456-426614174000").expect("uuid");
    let max = Uuid::max();
    let rows = vec![
        Row::upsert(vec![Value::Timestamp(base), Value::Uuid(max)]),
        Row::upsert(vec![
            Value::Timestamp(base + chrono::Duration::seconds(1)),
            Value::Uuid(v4),
        ]),
    ];
    sink.write_batch(
        &spec,
        &ctx,
        Batch {
            rows,
            next_cursor: None,
        },
        false,
    )
    .await
    .expect("write");

    // Poll until both rows visible.
    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_uuid_boundary")
            .fetch_one(&h.pool)
            .await
            .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, 2);

    // Spot-check the max value comes back identical.
    let row = sqlx::query("SELECT uid::string AS uid FROM bench_uuid_boundary ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select uids");
    assert_eq!(row.len(), 2);
    let got_max: String = row[0].try_get("uid").expect("uid 0");
    let got_v4: String = row[1].try_get("uid").expect("uid 1");
    assert_eq!(got_max.to_lowercase(), max.hyphenated().to_string());
    assert_eq!(got_v4.to_lowercase(), v4.hyphenated().to_string());

    h.drop_table("bench_uuid_boundary").await;
    h.pool.close().await;
}
