//! `SYMBOL` columns via pg-wire. The binding helper sends `SYMBOL` values
//! over the wire as TEXT; QuestDB coerces server-side because the DDL
//! declares the column as SYMBOL.

use chrono::{TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_questdb::types::symbol::QuestDbSymbolValue;
use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_symbol").await;
    h.exec(
        "CREATE TABLE bench_symbol ( \
            ts TIMESTAMP, \
            sym SYMBOL, \
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
    let spec = WriteSpec {
        table: "bench_symbol".to_string(),
        columns: vec!["ts".into(), "sym".into(), "v".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 5, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let rows = vec![
        Row::upsert(vec![
            Value::Timestamp(base),
            Value::Custom(Box::new(QuestDbSymbolValue("apple".into()))),
            Value::Float64(1.0),
        ]),
        Row::upsert(vec![
            Value::Timestamp(base + chrono::Duration::seconds(1)),
            Value::Custom(Box::new(QuestDbSymbolValue("banana".into()))),
            Value::Float64(2.0),
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

    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_symbol")
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

    // Round-trip read — symbols come back as TEXT over pg-wire.
    let rows = sqlx::query("SELECT sym FROM bench_symbol ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select sym");
    assert_eq!(rows.len(), 2);
    let got0: String = rows[0].try_get("sym").expect("sym 0");
    let got1: String = rows[1].try_get("sym").expect("sym 1");
    assert_eq!(got0, "apple");
    assert_eq!(got1, "banana");

    h.drop_table("bench_symbol").await;
    h.pool.close().await;
}

/// SYMBOL values with embedded whitespace, quotes, commas, newlines —
/// sqlx binds them as plain TEXT via pg-wire. Regression guard on
/// `bind_value_separated_pg`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn symbol_with_special_chars_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_symbol_special").await;
    h.exec(
        "CREATE TABLE bench_symbol_special ( \
            ts TIMESTAMP, \
            sym SYMBOL \
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
        table: "bench_symbol_special".to_string(),
        columns: vec!["ts".into(), "sym".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let base = Utc
        .with_ymd_and_hms(2025, 5, 2, 0, 0, 0)
        .single()
        .expect("ts");
    let payloads: Vec<&str> = vec![
        ",",
        "with space",
        "=eq",
        "with\nnewline",
        "with\"quote",
        "=symbol=",
    ];
    let rows: Vec<Row> = payloads
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Row::upsert(vec![
                Value::Timestamp(base + chrono::Duration::seconds(i as i64)),
                Value::Custom(Box::new(QuestDbSymbolValue((*s).to_string()))),
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

    let expected = payloads.len() as i64;
    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_symbol_special")
            .fetch_one(&h.pool)
            .await
            .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == expected {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(count, expected);

    // Round-trip check: read back in `ts ASC` order, compare to payloads.
    let rows = sqlx::query("SELECT sym FROM bench_symbol_special ORDER BY ts ASC")
        .fetch_all(&h.pool)
        .await
        .expect("select sym");
    assert_eq!(rows.len(), payloads.len());
    for (i, expected_sym) in payloads.iter().enumerate() {
        let got: String = rows[i].try_get("sym").expect("sym col");
        assert_eq!(
            &got, expected_sym,
            "symbol round-trip mismatch at row {i}: got {got:?}, want {expected_sym:?}"
        );
    }

    h.drop_table("bench_symbol_special").await;
    h.pool.close().await;
}
