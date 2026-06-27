//! NULL across multiple types via the pg-wire writer. sqlx binds typed
//! NULL through `bind_value_separated_pg`.

use chrono::{TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_double_and_text() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_nullable_basic").await;
    h.exec(
        "CREATE TABLE bench_nullable_basic ( \
            ts TIMESTAMP, \
            v DOUBLE, \
            s STRING \
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
        table: "bench_nullable_basic".to_string(),
        columns: vec!["ts".into(), "v".into(), "s".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let ts = Utc
        .with_ymd_and_hms(2025, 8, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![
        Value::Timestamp(ts),
        Value::Null,
        Value::Text("set".into()),
    ]);
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
        let row = sqlx::query("SELECT count() AS c FROM bench_nullable_basic WHERE v IS NULL")
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

    h.drop_table("bench_nullable_basic").await;
    h.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn null_binary() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_nullable_binary").await;
    h.exec(
        "CREATE TABLE bench_nullable_binary ( \
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
        table: "bench_nullable_binary".to_string(),
        columns: vec!["ts".into(), "payload".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let ts = Utc
        .with_ymd_and_hms(2025, 8, 2, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![Value::Timestamp(ts), Value::Null]);
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
        let row = sqlx::query("SELECT count() AS c FROM bench_nullable_binary")
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

    h.drop_table("bench_nullable_binary").await;
    h.pool.close().await;
}

/// NULL across LONG, UUID, and SYMBOL columns — one NULL per row keeps
/// the columns mutually independent so a per-type bind regression would
/// surface cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nulls_for_long_uuid_symbol() {
    use air_elt_commons_questdb::types::symbol::QuestDbSymbolValue;

    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_nullable_many").await;
    h.exec(
        "CREATE TABLE bench_nullable_many ( \
            ts TIMESTAMP, \
            i  LONG, \
            u  UUID, \
            s  SYMBOL, \
            keep DOUBLE \
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
        table: "bench_nullable_many".to_string(),
        columns: vec![
            "ts".into(),
            "i".into(),
            "u".into(),
            "s".into(),
            "keep".into(),
        ],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let base = Utc
        .with_ymd_and_hms(2025, 8, 3, 0, 0, 0)
        .single()
        .expect("ts");
    // Row 0 — LONG is NULL, others set.
    let row_long_null = Row::upsert(vec![
        Value::Timestamp(base),
        Value::Null,
        Value::Uuid(uuid::Uuid::max()),
        Value::Custom(Box::new(QuestDbSymbolValue("a".into()))),
        Value::Float64(1.0),
    ]);
    // Row 1 — UUID is NULL.
    let row_uuid_null = Row::upsert(vec![
        Value::Timestamp(base + chrono::Duration::seconds(1)),
        Value::Int64(42),
        Value::Null,
        Value::Custom(Box::new(QuestDbSymbolValue("b".into()))),
        Value::Float64(2.0),
    ]);
    // Row 2 — SYMBOL is NULL.
    let row_sym_null = Row::upsert(vec![
        Value::Timestamp(base + chrono::Duration::seconds(2)),
        Value::Int64(7),
        Value::Uuid(uuid::Uuid::nil()),
        Value::Null,
        Value::Float64(3.0),
    ]);
    sink.write_batch(
        &spec,
        &ctx,
        Batch {
            rows: vec![row_long_null, row_uuid_null, row_sym_null],
            next_cursor: None,
        },
        false,
    )
    .await
    .expect("write");

    let mut nulls = (0_i64, 0_i64, 0_i64);
    for _ in 0..50 {
        let n_i: i64 = sqlx::query("SELECT count() AS c FROM bench_nullable_many WHERE i IS NULL")
            .fetch_one(&h.pool)
            .await
            .expect("count i")
            .try_get("c")
            .expect("count decode");
        let n_u: i64 = sqlx::query("SELECT count() AS c FROM bench_nullable_many WHERE u IS NULL")
            .fetch_one(&h.pool)
            .await
            .expect("count u")
            .try_get("c")
            .expect("count decode");
        let n_s: i64 = sqlx::query("SELECT count() AS c FROM bench_nullable_many WHERE s IS NULL")
            .fetch_one(&h.pool)
            .await
            .expect("count s")
            .try_get("c")
            .expect("count decode");
        nulls = (n_i, n_u, n_s);
        if nulls == (1, 1, 1) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        nulls,
        (1, 1, 1),
        "expected one NULL per column (LONG / UUID / SYMBOL)"
    );

    h.drop_table("bench_nullable_many").await;
    h.pool.close().await;
}
