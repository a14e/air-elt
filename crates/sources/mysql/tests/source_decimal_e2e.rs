//! MySQL source e2e for `decimal` columns. Mirrors the PG decimal e2e —
//! schema introspection splits scale-0 vs scale-positive into BigInt vs
//! Decimal, and the decoder unwraps either via sqlx's `BigDecimal` path.
#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mysql::mysql_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_mysql::{MySqlSource, MySqlSourceConfig};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use sqlx::Executor;
use std::str::FromStr;

#[tokio::test]
async fn decimal_zero_scale_decoded_as_bigint_and_decimal() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE amounts (
                id INT NOT NULL PRIMARY KEY,
                big_count DECIMAL(30, 0) NOT NULL,
                rate DECIMAL(10, 4) NOT NULL,
                neg_big DECIMAL(30, 0) NOT NULL
             ) ENGINE=InnoDB",
        )
        .await
        .unwrap();

    let big_str = "1234567890123456789012345";
    let dec_str = "3.1416";
    let neg_str = "-9876543210987654321098765";
    sqlx::query("INSERT INTO amounts (id, big_count, rate, neg_big) VALUES (1, ?, ?, ?)")
        .bind(big_str)
        .bind(dec_str)
        .bind(neg_str)
        .execute(&handle.pool)
        .await
        .unwrap();

    let source = MySqlSource::connect(
        "test_source".to_string(),
        MySqlSourceConfig {
            url: handle.url_with_database(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let table = format!("{}.amounts", handle.schema);
    let schema = source.describe_schema(&table).await.unwrap();
    assert_eq!(
        schema.find("big_count").unwrap().data_type,
        DataType::BigInt { width: Some(30) }
    );
    assert_eq!(
        schema.find("rate").unwrap().data_type,
        DataType::Decimal {
            precision: Some(10),
            scale: Some(4)
        }
    );

    let spec = ReadSpec {
        columns: vec![
            "id".into(),
            "big_count".into(),
            "rate".into(),
            "neg_big".into(),
        ],
        table,
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
    };
    let ctx = source.build_context(&spec).await.unwrap();
    let batch = source.read_batch(&spec, ctx, None).await.unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(
        batch.rows[0].values[1],
        Value::BigInt(BigInt::from_str(big_str).unwrap())
    );
    assert_eq!(
        batch.rows[0].values[2],
        Value::Decimal(BigDecimal::from_str(dec_str).unwrap())
    );
    assert_eq!(
        batch.rows[0].values[3],
        Value::BigInt(BigInt::from_str(neg_str).unwrap()),
        "negative arbitrary-precision integer round-trips"
    );
}

/// MySQL UNSIGNED int columns map to `UInt8/16/32/64` and round-trip
/// through both `0` and exact-`{u8,u16,u32,u64}::MAX` values, plus a
/// `mediumint unsigned` column to lock the sqlx 24-bit→u32 decode path.
#[tokio::test]
async fn unsigned_int_columns_round_trip() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE counters (
                id INT NOT NULL PRIMARY KEY,
                u8_col   TINYINT UNSIGNED   NOT NULL,
                u16_col  SMALLINT UNSIGNED  NOT NULL,
                u24_col  MEDIUMINT UNSIGNED NOT NULL,
                u32_col  INT UNSIGNED       NOT NULL,
                u64_col  BIGINT UNSIGNED    NOT NULL
             ) ENGINE=InnoDB",
        )
        .await
        .unwrap();

    // Row 1: zero across the board — defends against "test passes only
    // because every value is in the upper half".
    sqlx::query(
        "INSERT INTO counters (id, u8_col, u16_col, u24_col, u32_col, u64_col) \
         VALUES (1, 0, 0, 0, 0, 0)",
    )
    .execute(&handle.pool)
    .await
    .unwrap();

    // Row 2: exact MAX for each width — would overflow corresponding signed.
    sqlx::query(
        "INSERT INTO counters (id, u8_col, u16_col, u24_col, u32_col, u64_col) \
         VALUES (2, ?, ?, ?, ?, ?)",
    )
    .bind(u8::MAX)
    .bind(u16::MAX)
    .bind(16_777_215_u32) // 2^24 - 1, mediumint unsigned max
    .bind(u32::MAX)
    .bind(u64::MAX)
    .execute(&handle.pool)
    .await
    .unwrap();

    let source = MySqlSource::connect(
        "test_source".to_string(),
        MySqlSourceConfig {
            url: handle.url_with_database(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let table = format!("{}.counters", handle.schema);
    let schema = source.describe_schema(&table).await.unwrap();
    assert_eq!(schema.find("u8_col").unwrap().data_type, DataType::UInt8);
    assert_eq!(schema.find("u16_col").unwrap().data_type, DataType::UInt16);
    // mediumint unsigned and int unsigned both canonicalise to UInt32.
    assert_eq!(schema.find("u24_col").unwrap().data_type, DataType::UInt32);
    assert_eq!(schema.find("u32_col").unwrap().data_type, DataType::UInt32);
    assert_eq!(schema.find("u64_col").unwrap().data_type, DataType::UInt64);

    let spec = ReadSpec {
        columns: vec![
            "id".into(),
            "u8_col".into(),
            "u16_col".into(),
            "u24_col".into(),
            "u32_col".into(),
            "u64_col".into(),
        ],
        table,
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
    };
    let ctx = source.build_context(&spec).await.unwrap();
    let batch = source.read_batch(&spec, ctx, None).await.unwrap();
    assert_eq!(batch.rows.len(), 2);

    // Row 1: zeros.
    assert_eq!(batch.rows[0].values[1], Value::UInt8(0));
    assert_eq!(batch.rows[0].values[2], Value::UInt16(0));
    assert_eq!(batch.rows[0].values[3], Value::UInt32(0));
    assert_eq!(batch.rows[0].values[4], Value::UInt32(0));
    assert_eq!(batch.rows[0].values[5], Value::UInt64(0));

    // Row 2: maxima.
    assert_eq!(batch.rows[1].values[1], Value::UInt8(u8::MAX));
    assert_eq!(batch.rows[1].values[2], Value::UInt16(u16::MAX));
    assert_eq!(batch.rows[1].values[3], Value::UInt32(16_777_215));
    assert_eq!(batch.rows[1].values[4], Value::UInt32(u32::MAX));
    assert_eq!(
        batch.rows[1].values[5],
        Value::UInt64(u64::MAX),
        "u64 wire-level max preserved (would overflow i64)"
    );
}
