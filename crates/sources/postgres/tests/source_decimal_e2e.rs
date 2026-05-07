//! PG source e2e for `numeric` columns. Exercises the schema-introspection
//! split between `BigInt` (scale 0) and `Decimal` (scale > 0), plus the
//! decoder paths for both.
#![allow(clippy::unwrap_used)]

use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use sqlx::Executor;
use std::str::FromStr;

#[tokio::test]
async fn numeric_zero_scale_decoded_as_bigint() {
    let handle = pg_pool().await;
    let ddl = format!(
        "CREATE TABLE {}.amounts (
            id INT NOT NULL PRIMARY KEY,
            big_count NUMERIC(30, 0) NOT NULL,
            unbounded NUMERIC
         )",
        handle.schema
    );
    handle.pool.execute(ddl.as_str()).await.unwrap();

    // 25-digit integer — bigger than i64.
    let big_str = "1234567890123456789012345";
    let dec_str = "3.14159265358979323846";
    sqlx::query(&format!(
        "INSERT INTO {}.amounts (id, big_count, unbounded) \
         VALUES (1, {}::numeric, {}::numeric)",
        handle.schema, big_str, dec_str
    ))
    .execute(&handle.pool)
    .await
    .unwrap();

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let table = format!("{}.amounts", handle.schema);
    let schema = source.describe_schema(&table).await.unwrap();
    assert_eq!(
        schema.find("big_count").unwrap().data_type,
        DataType::BigInt { width: Some(30) },
        "numeric(30, 0) → BigInt"
    );
    // Unmodified `numeric` is fully unbounded — schema reports both p,s as None.
    assert_eq!(
        schema.find("unbounded").unwrap().data_type,
        DataType::Decimal {
            precision: None,
            scale: None
        }
    );

    let spec = ReadSpec {
        columns: vec!["id".into(), "big_count".into(), "unbounded".into()],
        table,
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
    };
    let ctx = source.build_context(&spec).await.unwrap();
    let batch = source.read_batch(&spec, ctx, None).await.unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(
        batch.rows[0].values[1],
        Value::BigInt(BigInt::from_str(big_str).unwrap()),
        "scale-0 numeric round-trips as BigInt without going through BigDecimal arithmetic"
    );
    assert_eq!(
        batch.rows[0].values[2],
        Value::Decimal(BigDecimal::from_str(dec_str).unwrap())
    );
    handle.pool.close().await;
}

/// Cockroach mirror: a `DECIMAL(10, 2)` column round-trips as
/// `DataType::Decimal { precision: 10, scale: 2 }` and the bound value
/// preserves precision.
#[tokio::test]
async fn cockroach_decimal_round_trip() {
    let handle = cockroach_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE prices (
                id INT PRIMARY KEY,
                amount DECIMAL(10, 2) NOT NULL
            )",
        )
        .await
        .unwrap();

    let amount_str = "12345.67";
    sqlx::query("INSERT INTO prices (id, amount) VALUES (1, $1::DECIMAL(10, 2))")
        .bind(BigDecimal::from_str(amount_str).unwrap())
        .execute(&handle.pool)
        .await
        .unwrap();

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_database(),
            dialect: Dialect::Cockroach,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let table = "public.prices".to_string();
    let schema = source.describe_schema(&table).await.unwrap();
    assert_eq!(
        schema.find("amount").unwrap().data_type,
        DataType::Decimal {
            precision: Some(10),
            scale: Some(2),
        }
    );

    let spec = ReadSpec {
        columns: vec!["id".into(), "amount".into()],
        table,
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
    };
    let ctx = source.build_context(&spec).await.unwrap();
    let batch = source.read_batch(&spec, ctx, None).await.unwrap();
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(
        batch.rows[0].values[1],
        Value::Decimal(BigDecimal::from_str(amount_str).unwrap())
    );
    handle.pool.close().await;
}
