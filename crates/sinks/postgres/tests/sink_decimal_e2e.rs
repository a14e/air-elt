//! PG sink e2e for `numeric` columns. Asserts that BigInt and Decimal
//! values both reach the database byte-exact.
#![allow(clippy::unwrap_used)]

use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use sqlx::Executor;
use std::str::FromStr;

#[tokio::test]
async fn writes_numeric_bigint_and_decimal() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            format!(
                "CREATE TABLE {}.t (
                    id INT NOT NULL PRIMARY KEY,
                    big NUMERIC(30, 0) NOT NULL,
                    rate NUMERIC(10, 4) NOT NULL
                 )",
                handle.schema
            )
            .as_str(),
        )
        .await
        .unwrap();

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .unwrap();

    let spec = WriteSpec {
        columns: vec!["id".into(), "big".into(), "rate".into()],
        table: format!("{}.t", handle.schema),
        conflict: None,
    };

    let big_str = "9876543210987654321098765";
    let rate_str = "1.2345";
    let batch = Batch {
        rows: vec![CoreRow::upsert(vec![
            Value::Int32(1),
            Value::BigInt(BigInt::from_str(big_str).unwrap()),
            Value::Decimal(BigDecimal::from_str(rate_str).unwrap()),
        ])],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.unwrap();
    let report = sink.write_batch(&spec, ctx, batch, false).await.unwrap();
    assert_eq!(report.rows_written, 1);

    let (big, rate): (BigDecimal, BigDecimal) = sqlx::query_as(&format!(
        "SELECT big, rate FROM {}.t WHERE id = 1",
        handle.schema
    ))
    .fetch_one(&handle.pool)
    .await
    .unwrap();
    assert_eq!(big, BigDecimal::from_str(big_str).unwrap());
    assert_eq!(rate, BigDecimal::from_str(rate_str).unwrap());
    handle.pool.close().await;
}

/// Cockroach round-trip for `DECIMAL(10, 2)`. CockroachDB exposes the same
/// `DECIMAL` type as Postgres; the sink must bind `Value::Decimal` byte-exact.
#[tokio::test]
async fn cockroach_writes_numeric_decimal() {
    let handle = cockroach_pool().await;
    handle
        .pool
        .execute("CREATE TABLE rates (id INT NOT NULL PRIMARY KEY, rate DECIMAL(10, 2) NOT NULL)")
        .await
        .unwrap();

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_database(),
        dialect: Dialect::Cockroach,
        ..Default::default()
    })
    .await
    .unwrap();

    let spec = WriteSpec {
        columns: vec!["id".into(), "rate".into()],
        table: "rates".into(),
        conflict: None,
    };

    let rate_str = "12345678.90";
    let batch = Batch {
        rows: vec![CoreRow::upsert(vec![
            Value::Int64(1),
            Value::Decimal(BigDecimal::from_str(rate_str).unwrap()),
        ])],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.unwrap();
    let report = sink.write_batch(&spec, ctx, batch, false).await.unwrap();
    assert_eq!(report.rows_written, 1);

    let (rate,): (BigDecimal,) = sqlx::query_as("SELECT rate FROM rates WHERE id = 1")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(rate, BigDecimal::from_str(rate_str).unwrap());
    handle.pool.close().await;
}
