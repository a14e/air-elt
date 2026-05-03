//! `[flow.<name>.conflict]` translates into `ON CONFLICT … DO {NOTHING|UPDATE}`
//! at the PG sink. Used to live in `crates/app/tests/pg_to_pg.rs`, but it
//! exercises pg-sink semantics, not runner wiring — moving it next to the
//! sink keeps app tests focused on cross-vendor glue and avoids spinning
//! up storage just to assert SQL behaviour.
#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use sqlx::Executor;

async fn seed_items(pool: &sqlx::PgPool) {
    pool.execute(
        "CREATE TABLE items (
            id BIGINT PRIMARY KEY,
            label TEXT NOT NULL
        )",
    )
    .await
    .unwrap();
}

fn batch(rows: &[(i64, &str)]) -> Batch {
    Batch {
        rows: rows
            .iter()
            .map(|(id, label)| CoreRow {
                values: vec![Value::Int64(*id), Value::Text((*label).into())],
            })
            .collect(),
        next_cursor: None,
    }
}

async fn fetch_labels(pool: &sqlx::PgPool) -> Vec<(i64, String)> {
    sqlx::query_as("SELECT id, label FROM items ORDER BY id")
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn connect(handle: &air_elt_commons_testing::pg::PgTestHandle) -> PgSink {
    PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink")
}

fn spec(
    handle: &air_elt_commons_testing::pg::PgTestHandle,
    strategy: ConflictStrategy,
) -> WriteSpec {
    WriteSpec {
        columns: vec!["id".into(), "label".into()],
        table: format!("{}.items", handle.schema),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy,
        }),
    }
}

#[tokio::test]
async fn overwrite_replaces_existing_rows() {
    let handle = pg_pool().await;
    seed_items(&handle.pool).await;

    // Pre-seed with stale labels — the sink's UPSERT must replace them.
    for i in 1_i64..=3 {
        sqlx::query("INSERT INTO items (id, label) VALUES ($1, $2)")
            .bind(i)
            .bind(format!("stale-{i}"))
            .execute(&handle.pool)
            .await
            .unwrap();
    }

    let sink = connect(&handle).await;
    let s = spec(&handle, ConflictStrategy::Overwrite);
    let ctx = sink.build_context(&s).await.expect("build_context");
    let payload = batch(&[(1, "fresh-1"), (2, "fresh-2"), (3, "fresh-3")]);
    let report = sink.write_batch(&s, ctx, &payload).await.expect("write");
    assert_eq!(report.rows_written, 3);

    let rows = fetch_labels(&handle.pool).await;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0], (1, "fresh-1".into()));
    assert_eq!(rows[2], (3, "fresh-3".into()));
}

#[tokio::test]
async fn ignore_preserves_existing_rows() {
    let handle = pg_pool().await;
    seed_items(&handle.pool).await;

    for i in 1_i64..=3 {
        sqlx::query("INSERT INTO items (id, label) VALUES ($1, $2)")
            .bind(i)
            .bind(format!("kept-{i}"))
            .execute(&handle.pool)
            .await
            .unwrap();
    }

    let sink = connect(&handle).await;
    let s = spec(&handle, ConflictStrategy::Ignore);
    let ctx = sink.build_context(&s).await.expect("build_context");
    let payload = batch(&[(1, "drop-1"), (2, "drop-2"), (3, "drop-3")]);
    sink.write_batch(&s, ctx, &payload).await.expect("write");

    let rows = fetch_labels(&handle.pool).await;
    assert_eq!(rows.len(), 3);
    // ignore: the pre-existing rows survive untouched.
    assert_eq!(rows[0], (1, "kept-1".into()));
    assert_eq!(rows[2], (3, "kept-3".into()));
}

#[tokio::test]
async fn overwrite_is_idempotent_on_rerun() {
    let handle = pg_pool().await;
    seed_items(&handle.pool).await;

    let sink = connect(&handle).await;
    let s = spec(&handle, ConflictStrategy::Overwrite);
    let ctx = sink.build_context(&s).await.expect("build_context");
    let payload = batch(&[(1, "v1"), (2, "v2")]);

    sink.write_batch(&s, ctx.clone(), &payload)
        .await
        .expect("first");
    sink.write_batch(&s, ctx, &payload).await.expect("rerun");

    let rows = fetch_labels(&handle.pool).await;
    assert_eq!(rows.len(), 2, "re-run must not duplicate");
    assert_eq!(rows[0], (1, "v1".into()));
}
