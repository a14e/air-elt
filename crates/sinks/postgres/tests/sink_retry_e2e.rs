//! Verify that the PG sink retries on CockroachDB's `40001 RETRY_SERIALIZABLE`.
//!
//! CockroachDB defaults to SERIALIZABLE isolation. When two transactions
//! collide on the same row, one of them is asked to restart. The sink wraps
//! its writes in [`air_elt_commons_pg::retry::with_serialization_retry`], so
//! the operator never sees the transient error.
//!
//! The test fires two concurrent `write_batch` calls that overlap on the
//! primary key. With single-key Overwrite the sink uses Cockroach's native
//! `UPSERT`, which under contention will return `40001` from the loser; the
//! retry wrapper must re-run the statement. We assert both calls succeed and
//! the final value is one of the two writers' payloads.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cockroach_retries_on_serialization_failure() {
    let handle = cockroach_pool().await;
    handle
        .pool
        .execute("CREATE TABLE seq (id INT PRIMARY KEY, n INT NOT NULL)")
        .await
        .unwrap();
    sqlx::query("INSERT INTO seq (id, n) VALUES (1, 0)")
        .execute(&handle.pool)
        .await
        .unwrap();

    // Wrap the sink in Arc so two tokio tasks can share it. PgSink itself is
    // already `Send + Sync` (PgPool is internally an Arc).
    let sink = Arc::new(
        PgSink::connect(PgSinkConfig {
            url: handle.url_with_database(),
            dialect: Dialect::Cockroach,
            ..Default::default()
        })
        .await
        .unwrap(),
    );

    let spec = Arc::new(WriteSpec {
        columns: vec!["id".into(), "n".into()],
        table: "seq".into(),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    });
    let ctx = sink.build_context(&spec).await.unwrap();

    // Two concurrent writers targeting the same PK. With SERIALIZABLE
    // isolation Cockroach will force one of them to restart; the sink's
    // retry wrapper must absorb the 40001 transparently.
    let mut handles = Vec::new();
    for n in [10_i64, 20_i64] {
        let sink = Arc::clone(&sink);
        let spec = Arc::clone(&spec);
        let ctx = Arc::clone(&ctx);
        handles.push(tokio::spawn(async move {
            let batch = Batch {
                rows: vec![CoreRow::upsert(vec![Value::Int64(1), Value::Int64(n)])],
                next_cursor: None,
            };
            sink.write_batch(&spec, ctx, batch, false).await
        }));
    }

    for h in handles {
        h.await.expect("task join").expect("write_batch succeeded");
    }

    let (id, n): (i64, i64) = sqlx::query_as("SELECT id, n FROM seq WHERE id = 1")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(id, 1);
    // Last writer wins — must be one of the two values written.
    assert!(n == 10 || n == 20, "unexpected final value: {n}");
    handle.pool.close().await;
}
