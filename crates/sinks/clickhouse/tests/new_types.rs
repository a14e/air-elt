//! E2e round-trip tests for Decimal, Int128/UInt128, Int256/UInt256.
//!
//! Each test:
//!  1. Creates a table with the relevant CH column type.
//!  2. Inserts one row via `ChSink::write_batch` (RowBinary path).
//!  3. Reads the value back via a direct SQL query.
//!  4. Asserts that CH received the correct value.
//!
//! `Decimal*` uses `Value::Decimal(BigDecimal)` through the canonical
//! `DataType::Decimal` path added in this changeset.
//!
//! `Int128` / `UInt128` use `Value::Custom(ChInt128Value)` /
//! `Value::Custom(ChUInt128Value)` through `DataType::Custom(ChInt128Type)`
//! / `DataType::Custom(ChUInt128Type)`.
//!
//! `Int256` / `UInt256` use `Value::Custom(ChInt256Value)` /
//! `Value::Custom(ChUInt256Value)` through `DataType::Custom(ChInt256Type)`
//! / `DataType::Custom(ChUInt256Type)`.

use bigdecimal::BigDecimal;
use num_bigint::{BigInt, BigUint};

use air_elt_commons_clickhouse::types::int128::{ChInt128Value, ChUInt128Value};
use air_elt_commons_clickhouse::types::int256::{
    ChInt256Value, ChUInt256Value, bigint_to_le32, biguint_to_le32,
};
use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_clickhouse::{ChSink, ChSinkConfig};

// ---------------------------------------------------------------- Decimal

#[tokio::test]
async fn round_trip_decimal32() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE dec32_t (id UInt64, v Decimal32(2)) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "dec32_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let d: BigDecimal = "12.34".parse().expect("valid decimal");
    let batch = Batch {
        rows: vec![Row {
            values: vec![Value::UInt64(1), Value::Decimal(d)],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written, 1);

    let body = h
        .exec("SELECT toString(v) FROM dec32_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, "12.34", "Decimal32 round-trip: got {val:?}");
}

#[tokio::test]
async fn round_trip_decimal64() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE dec64_t (id UInt64, v Decimal64(4)) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "dec64_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let d: BigDecimal = "1.0001".parse().expect("valid decimal");
    let batch = Batch {
        rows: vec![Row {
            values: vec![Value::UInt64(1), Value::Decimal(d)],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM dec64_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, "1.0001", "Decimal64 round-trip: got {val:?}");
}

#[tokio::test]
async fn round_trip_decimal128() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE dec128_t (id UInt64, v Decimal128(6)) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "dec128_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let d: BigDecimal = "9876543.210001".parse().expect("valid decimal");
    let batch = Batch {
        rows: vec![Row {
            values: vec![Value::UInt64(1), Value::Decimal(d)],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM dec128_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, "9876543.210001", "Decimal128 round-trip: got {val:?}");
}

#[tokio::test]
async fn round_trip_decimal_negative() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE decneg_t (id UInt64, v Decimal32(2)) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "decneg_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let d: BigDecimal = "-99.99".parse().expect("valid decimal");
    let batch = Batch {
        rows: vec![Row {
            values: vec![Value::UInt64(1), Value::Decimal(d)],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM decneg_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, "-99.99", "negative Decimal32 round-trip: got {val:?}");
}

// ---------------------------------------------------------------- Int128 / UInt128

#[tokio::test]
async fn round_trip_int128() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE int128_t (id UInt64, v Int128) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "int128_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // A value that doesn't fit in i64: 2^70
    let n: i128 = 1_180_591_620_717_411_303_424_i128; // 2^70
    let batch = Batch {
        rows: vec![Row {
            values: vec![Value::UInt64(1), Value::Custom(Box::new(ChInt128Value(n)))],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM int128_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, n.to_string(), "Int128 round-trip: got {val:?}");
}

#[tokio::test]
async fn round_trip_uint128() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE uint128_t (id UInt64, v UInt128) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "uint128_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // 2^100 — doesn't fit in u64.
    let n: u128 = 1_267_650_600_228_229_401_496_703_205_376_u128; // 2^100
    let batch = Batch {
        rows: vec![Row {
            values: vec![Value::UInt64(1), Value::Custom(Box::new(ChUInt128Value(n)))],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM uint128_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, n.to_string(), "UInt128 round-trip: got {val:?}");
}

// ---------------------------------------------------------------- Int256 / UInt256

#[tokio::test]
async fn round_trip_int256() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE int256_t (id UInt64, v Int256) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "int256_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // 2^200 + 1 — does not fit in i128.
    let n = BigInt::parse_bytes(
        b"1606938044258990275541962092341162602522202993782792835301377",
        10,
    )
    .expect("valid big int");
    let le_bytes = bigint_to_le32(&n).expect("Int256 value fits in 256 bits");
    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChInt256Value { le_bytes })),
            ],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM int256_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, n.to_string(), "Int256 round-trip: got {val:?}");
}

#[tokio::test]
async fn round_trip_uint256() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE uint256_t (id UInt64, v UInt256) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");
    let spec = WriteSpec {
        table: "uint256_t".to_string(),
        columns: vec!["id".into(), "v".into()],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // 2^200 — does not fit in u128.
    let n = BigUint::parse_bytes(
        b"1606938044258990275541962092341162602522202993782792835301376",
        10,
    )
    .expect("valid big uint");
    let le_bytes = biguint_to_le32(&n).expect("UInt256 value fits in 256 bits");
    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChUInt256Value { le_bytes })),
            ],
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(v) FROM uint256_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    let val = body.trim();
    assert_eq!(val, n.to_string(), "UInt256 round-trip: got {val:?}");
}
