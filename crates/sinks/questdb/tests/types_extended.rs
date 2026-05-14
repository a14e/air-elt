//! Type round-trips that exercise less-common pg-wire bind paths:
//! BINARY, DATE, LONG256, IPv4, GEOHASH.

use chrono::{NaiveDate, TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_questdb::types::geohash::QuestDbGeohashValue;
use air_elt_commons_questdb::types::ipv4::QuestDbIpv4Value;
use air_elt_commons_questdb::types::long256::QuestDbLong256Value;
use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};
use std::net::Ipv4Addr;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_types_binary").await;
    h.exec(
        "CREATE TABLE bench_types_binary ( \
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
        table: "bench_types_binary".to_string(),
        columns: vec!["ts".into(), "payload".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2025, 7, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![Value::Timestamp(ts), Value::Bytes(vec![1, 2, 3, 4])]);
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
        .expect("write_batch");
    assert_eq!(report.rows_written, 1);

    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS c FROM bench_types_binary")
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

    // Read-back: payload bytes must match what was written.
    let row = sqlx::query("SELECT payload FROM bench_types_binary")
        .fetch_one(&h.pool)
        .await
        .expect("select payload");
    let got: Vec<u8> = row.try_get("payload").expect("payload decode");
    assert_eq!(got, vec![1, 2, 3, 4]);

    h.drop_table("bench_types_binary").await;
    h.pool.close().await;
}

/// `DATE` column. The pg_bind path coerces `NaiveDate` → server-side
/// wall date.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn date_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_date").await;
    h.exec(
        "CREATE TABLE bench_date ( \
            ts TIMESTAMP, \
            d  DATE \
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
        table: "bench_date".to_string(),
        columns: vec!["ts".into(), "d".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2026, 4, 1, 0, 0, 0)
        .single()
        .expect("ts");
    let d = NaiveDate::from_ymd_opt(2026, 4, 1).expect("date");
    let row = Row::upsert(vec![Value::Timestamp(ts), Value::Date(d)]);
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
        let row = sqlx::query("SELECT count() AS c FROM bench_date")
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

    // Read-back: stringify DATE column and compare to the exact wire
    // format. QuestDB DATE is millisecond wall time, rendered as
    // `YYYY-MM-DDThh:mm:ss.sssZ`. The binder coerces `NaiveDate` to
    // start-of-day UTC, so 2026-04-01 lands as the literal below.
    let row = sqlx::query("SELECT d::string AS d FROM bench_date")
        .fetch_one(&h.pool)
        .await
        .expect("select d");
    let got: String = row.try_get("d").expect("d decode");
    assert_eq!(
        got, "2026-04-01T00:00:00.000Z",
        "expected exact DATE wire format, got {got:?}"
    );

    h.drop_table("bench_date").await;
    h.pool.close().await;
}

/// `LONG256` round-trip. The value is rendered as `0x...` 64-hex-char text
/// and QuestDB casts to LONG256 server-side.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn long256_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_long256").await;
    h.exec(
        "CREATE TABLE bench_long256 ( \
            ts TIMESTAMP, \
            v  LONG256 \
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
        table: "bench_long256".to_string(),
        columns: vec!["ts".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // 32 LE bytes with a non-zero high byte so the rendered hex is
    // recognisably different from the all-zero baseline.
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = i as u8;
    }
    let ts = Utc
        .with_ymd_and_hms(2026, 4, 2, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![
        Value::Timestamp(ts),
        Value::Custom(Box::new(QuestDbLong256Value(bytes))),
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
        let row = sqlx::query("SELECT count() AS c FROM bench_long256")
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

    // Read-back: stringify LONG256 and compare to "0x" + big-endian hex.
    let expected_hex = QuestDbLong256Value(bytes).to_hex();
    let row = sqlx::query("SELECT v::string AS v FROM bench_long256")
        .fetch_one(&h.pool)
        .await
        .expect("select v");
    let got: String = row.try_get("v").expect("v decode");
    assert_eq!(got.to_lowercase(), expected_hex);

    h.drop_table("bench_long256").await;
    h.pool.close().await;
}

/// `IPv4` round-trip — dotted-quad text on the wire.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipv4_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_ipv4").await;
    h.exec(
        "CREATE TABLE bench_ipv4 ( \
            ts TIMESTAMP, \
            ip IPv4 \
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
        table: "bench_ipv4".to_string(),
        columns: vec!["ts".into(), "ip".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let ts = Utc
        .with_ymd_and_hms(2026, 4, 3, 0, 0, 0)
        .single()
        .expect("ts");
    let addr = Ipv4Addr::new(203, 0, 113, 42);
    let row = Row::upsert(vec![
        Value::Timestamp(ts),
        Value::Custom(Box::new(QuestDbIpv4Value(addr))),
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
        let row = sqlx::query("SELECT count() AS c FROM bench_ipv4 WHERE ip = '203.0.113.42'")
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

    // Read-back: explicit comparison against the dotted-quad text.
    let row = sqlx::query("SELECT ip::string AS ip FROM bench_ipv4")
        .fetch_one(&h.pool)
        .await
        .expect("select ip");
    let got: String = row.try_get("ip").expect("ip decode");
    assert_eq!(got, "203.0.113.42");

    h.drop_table("bench_ipv4").await;
    h.pool.close().await;
}

/// `GEOHASH(7c)` (35 bits) round-trip — base32 text "u4pruyd".
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn geohash_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    h.drop_table("bench_geohash").await;
    h.exec(
        "CREATE TABLE bench_geohash ( \
            ts TIMESTAMP, \
            g  GEOHASH(7c) \
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
        table: "bench_geohash".to_string(),
        columns: vec!["ts".into(), "g".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Decode "u4pruyd" into the packed 35-bit value the writer will
    // re-encode back to base32 over the wire. Same alphabet QuestDB uses.
    const ALPHABET: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";
    let geohash_chars = b"u4pruyd";
    let mut packed: u64 = 0;
    for &c in geohash_chars {
        let idx = ALPHABET
            .iter()
            .position(|&a| a == c)
            .expect("alphabet member") as u64;
        packed = (packed << 5) | idx;
    }

    let ts = Utc
        .with_ymd_and_hms(2026, 4, 4, 0, 0, 0)
        .single()
        .expect("ts");
    let row = Row::upsert(vec![
        Value::Timestamp(ts),
        Value::Custom(Box::new(QuestDbGeohashValue {
            bits: 35,
            value: packed,
        })),
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
        let row = sqlx::query("SELECT count() AS c FROM bench_geohash")
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

    // Read-back: GEOHASH does not expose a cast-to-string over pg-wire,
    // so filter on the textual literal — a non-match would surface as a
    // zero-row result and fail the count below.
    let row = sqlx::query("SELECT count() AS c FROM bench_geohash WHERE g = #u4pruyd")
        .fetch_one(&h.pool)
        .await
        .expect("select g");
    let got: i64 = row.try_get("c").expect("c decode");
    assert_eq!(got, 1);

    h.drop_table("bench_geohash").await;
    h.pool.close().await;
}
