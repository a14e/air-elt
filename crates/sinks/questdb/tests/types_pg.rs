//! Round-trips for declared-supported canonical types over pg-wire:
//! `Bool`, `Int8`, `Int16`, `Float32`, and `CHAR`.
//!
//! These types are accepted by `type_supported` but were missing
//! end-to-end coverage; the regression risk shows up in the type-matrix
//! widening rules and in `bind_value_separated_pg`'s typed-NULL arms.

use chrono::{TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

async fn poll_count(pool: &sqlx::PgPool, table: &str, expected: i64) -> i64 {
    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query(&format!("SELECT count() AS c FROM {table}"))
            .fetch_one(pool)
            .await
            .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == expected {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    count
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bool_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_bool").await;
    h.exec(
        "CREATE TABLE bench_bool ( \
            ts TIMESTAMP, \
            b  BOOLEAN \
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
        table: "bench_bool".to_string(),
        columns: vec!["ts".into(), "b".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 6, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let rows = vec![
        Row::upsert(vec![Value::Timestamp(base), Value::Bool(true)]),
        Row::upsert(vec![
            Value::Timestamp(base + chrono::Duration::seconds(1)),
            Value::Bool(false),
        ]),
        // QuestDB BOOLEAN is non-nullable server-side (NULL → FALSE on
        // insert), so the NULL row simply lands as `false`.
        Row::upsert(vec![
            Value::Timestamp(base + chrono::Duration::seconds(2)),
            Value::Null,
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

    assert_eq!(poll_count(&h.pool, "bench_bool", 3).await, 3);

    let rows = sqlx::query("SELECT b FROM bench_bool ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select b");
    let got: Vec<bool> = rows.iter().map(|r| r.try_get("b").expect("b")).collect();
    assert_eq!(got, vec![true, false, false]);

    h.drop_table("bench_bool").await;
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn int8_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_int8").await;
    h.exec(
        "CREATE TABLE bench_int8 ( \
            ts TIMESTAMP, \
            v  BYTE \
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
        table: "bench_int8".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 6, 2, 0, 0, 0)
        .single()
        .expect("ts");
    let values: Vec<i8> = vec![i8::MIN, -1, 0, i8::MAX];
    let rows: Vec<Row> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Row::upsert(vec![
                Value::Timestamp(base + chrono::Duration::seconds(i as i64)),
                Value::Int8(*v),
            ])
        })
        .collect();
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

    assert_eq!(poll_count(&h.pool, "bench_int8", 4).await, 4);
    let rows = sqlx::query("SELECT v FROM bench_int8 ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select v");
    let got: Vec<i16> = rows.iter().map(|r| r.try_get("v").expect("v")).collect();
    let expected: Vec<i16> = values.iter().map(|x| *x as i16).collect();
    assert_eq!(got, expected);

    h.drop_table("bench_int8").await;
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn int16_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_int16").await;
    h.exec(
        "CREATE TABLE bench_int16 ( \
            ts TIMESTAMP, \
            v  SHORT \
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
        table: "bench_int16".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 6, 3, 0, 0, 0)
        .single()
        .expect("ts");
    let values: Vec<i16> = vec![i16::MIN, -1, 0, i16::MAX];
    let rows: Vec<Row> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            Row::upsert(vec![
                Value::Timestamp(base + chrono::Duration::seconds(i as i64)),
                Value::Int16(*v),
            ])
        })
        .collect();
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

    assert_eq!(poll_count(&h.pool, "bench_int16", 4).await, 4);
    let rows = sqlx::query("SELECT v FROM bench_int16 ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select v");
    let got: Vec<i16> = rows.iter().map(|r| r.try_get("v").expect("v")).collect();
    assert_eq!(got, values);

    h.drop_table("bench_int16").await;
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn float32_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_float32").await;
    h.exec(
        "CREATE TABLE bench_float32 ( \
            ts TIMESTAMP, \
            v  FLOAT \
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
        table: "bench_float32".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 6, 4, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![Value::Timestamp(base), Value::Float32(2.5_f32)]);
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

    assert_eq!(poll_count(&h.pool, "bench_float32", 1).await, 1);
    let row = sqlx::query("SELECT v FROM bench_float32")
        .fetch_one(&h.pool)
        .await
        .expect("select v");
    let got: f32 = row.try_get("v").expect("v");
    assert!((got - 2.5_f32).abs() < 1e-6);

    h.drop_table("bench_float32").await;
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn char_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_char").await;
    h.exec(
        "CREATE TABLE bench_char ( \
            ts TIMESTAMP, \
            c  CHAR \
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
        table: "bench_char".to_string(),
        columns: vec!["ts".into(), "c".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 6, 5, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![Value::Timestamp(base), Value::Text("Z".into())]);
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

    assert_eq!(poll_count(&h.pool, "bench_char", 1).await, 1);
    let row = sqlx::query("SELECT c::string AS c FROM bench_char")
        .fetch_one(&h.pool)
        .await
        .expect("select c");
    let got: String = row.try_get("c").expect("c");
    assert_eq!(got, "Z");

    h.drop_table("bench_char").await;
    h.pool.close().await;
}
