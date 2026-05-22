//! Dry-run path for `PgSink::write_batch` (T6).
//!
//! `dry_run = true` must build the same SQL as production (planner
//! parses every bind, types are checked) but the `WHERE false`
//! short-circuit means the table stays empty and `rows_written = 0`.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use sqlx::Executor;

#[tokio::test]
async fn write_batch_dry_run_skips_writes_then_real_run_commits() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_t (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("create dry_run_t");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.dry_run_t", handle.schema),
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");

    let make_batch = || Batch {
        rows: vec![
            CoreRow::upsert(vec![Value::Int32(1), Value::Text("alice".into())]),
            CoreRow::upsert(vec![Value::Int32(2), Value::Text("bob".into())]),
        ],
        next_cursor: None,
    };

    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Dry-run: SQL parses + binds, but `WHERE false` keeps the table
    // empty and the report reads zero rows written.
    let report_dry = sink
        .write_batch(&spec, &ctx, make_batch(), true)
        .await
        .expect("dry-run write");
    assert_eq!(report_dry.rows_written(), 0);
    let count_after_dry: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(
        count_after_dry, 0,
        "dry-run must leave the target table empty"
    );

    // Real run: same batch lands two rows.
    let report = sink
        .write_batch(&spec, &ctx, make_batch(), false)
        .await
        .expect("real write");
    assert_eq!(report.rows_written(), 2);
    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_after, 2);

    handle.pool.close().await;
}

/// Negative case: a row whose `Value::Text` is bound against an
/// `INTEGER` column. The Postgres planner type-checks the projection
/// before the `WHERE false` short-circuit, so the dry-run path must
/// surface the type mismatch as `Err` even though no row would ever
/// be written. This is what proves the server actually parses every
/// bind — a hypothetical client-side `Ok(0)` short-circuit would not.
#[tokio::test]
async fn write_batch_dry_run_rejects_type_mismatch_server_side() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_neg_t (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("create dry_run_neg_t");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.dry_run_neg_t", handle.schema),
        conflict: None,
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // `Value::Text("not-an-int")` against the INTEGER column. The bind
    // ships as `text` OID; pg's planner refuses the implicit cast.
    let bad_batch = Batch {
        rows: vec![CoreRow::upsert(vec![
            Value::Text("not-an-int".into()),
            Value::Text("alice".into()),
        ])],
        next_cursor: None,
    };

    let err = sink
        .write_batch(&spec, &ctx, bad_batch, true)
        .await
        .expect_err("dry-run must reject server-side type mismatch");
    let msg = format!("{err:#}");
    // Sanity-check: the error originates from the pg server, not from
    // a local client-side guard.
    assert!(
        msg.to_lowercase().contains("integer")
            || msg.to_lowercase().contains("type")
            || msg.to_lowercase().contains("invalid"),
        "expected pg type-mismatch error, got: {msg}"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_neg_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "negative dry-run must not write any rows");

    handle.pool.close().await;
}

/// Mixed Upsert + Delete dry-run: the production runner ships both
/// op kinds in the same batch, so the dry-run path must exercise both
/// `write_upsert_batch_dry` and `write_delete_batch_dry`. Negative key
/// variant — a `Delete` row with a `Value::Text` against the INTEGER
/// key — proves the dry-run delete path actually binds keys server-side.
#[tokio::test]
async fn write_batch_dry_run_delete_rejects_type_mismatch_server_side() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_del_t (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("create dry_run_del_t");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.dry_run_del_t", handle.schema),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Sanity: a well-typed mixed batch (upsert + delete) goes through
    // dry-run cleanly and leaves the table empty.
    let mixed_ok = Batch {
        rows: vec![
            CoreRow::upsert(vec![Value::Int32(1), Value::Text("alice".into())]),
            CoreRow::delete(vec![Value::Int32(2), Value::Text("ignored".into())]),
        ],
        next_cursor: None,
    };
    let report_ok = sink
        .write_batch(&spec, &ctx, mixed_ok, true)
        .await
        .expect("mixed dry-run");
    assert_eq!(report_ok.rows_written(), 0);
    let count_ok: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_del_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_ok, 0, "mixed dry-run must leave the target empty");

    // Negative: a Delete row whose key column is `Value::Text` against
    // an INTEGER key. The dry-run delete path emits
    // `DELETE FROM ... WHERE (id) IN ($1) AND false` — pg refuses the
    // text-to-integer cast at plan time.
    let bad_delete = Batch {
        rows: vec![CoreRow::delete(vec![
            Value::Text("not-an-int".into()),
            Value::Text("ignored".into()),
        ])],
        next_cursor: None,
    };
    let err = sink
        .write_batch(&spec, &ctx, bad_delete, true)
        .await
        .expect_err("dry-run delete must reject server-side type mismatch");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("integer")
            || msg.contains("type")
            || msg.contains("invalid")
            || msg.contains("binary data format"),
        "expected pg type-mismatch error, got: {msg}"
    );

    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_del_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_after, 0, "negative delete dry-run must not write");

    handle.pool.close().await;
}

/// Delete dry-run against seeded rows: proves the dry-run delete path
/// is a true no-op against pre-existing data, and that the same batch
/// in production mode actually removes the rows. Closes the gap where
/// the existing delete dry-run test only ran against an empty target
/// (where `Ok(0)` short-circuit would also have passed).
#[tokio::test]
async fn write_batch_dry_run_delete_preserves_then_real_run_deletes() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_del_seeded_t (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("create dry_run_del_seeded_t");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.dry_run_del_seeded_t", handle.schema),
        conflict: Some(ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Seed two rows via the real upsert path.
    let seed = Batch {
        rows: vec![
            CoreRow::upsert(vec![Value::Int32(1), Value::Text("alice".into())]),
            CoreRow::upsert(vec![Value::Int32(2), Value::Text("bob".into())]),
        ],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, seed, false)
        .await
        .expect("seed upsert");
    let count_seeded: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_del_seeded_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_seeded, 2);

    // Dry-run delete with valid keys: must not remove anything.
    let make_delete = || Batch {
        rows: vec![
            CoreRow::delete(vec![Value::Int32(1), Value::Text("ignored".into())]),
            CoreRow::delete(vec![Value::Int32(2), Value::Text("ignored".into())]),
        ],
        next_cursor: None,
    };
    let report_dry = sink
        .write_batch(&spec, &ctx, make_delete(), true)
        .await
        .expect("dry-run delete");
    assert_eq!(report_dry.rows_written(), 0);
    let count_after_dry: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_del_seeded_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(
        count_after_dry, 2,
        "dry-run delete must leave seeded rows in place"
    );

    // Real delete: same batch removes both rows.
    let report_real = sink
        .write_batch(&spec, &ctx, make_delete(), false)
        .await
        .expect("real delete");
    assert_eq!(report_real.rows_written(), 2);
    let count_after_real: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_del_seeded_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_after_real, 0, "real delete must remove seeded rows");

    handle.pool.close().await;
}

/// Negative: dry-run upsert with a `conflict.key` that names an
/// existing column with no UNIQUE/PRIMARY KEY constraint backing it.
/// Postgres parses and validates the `ON CONFLICT (...)` clause against
/// the INSERT target's indexes regardless of the `WHERE false`
/// short-circuit, so a misconfigured conflict key must surface during
/// validate=true rather than only on first real write. Without the
/// dry-run builder appending the conflict suffix, this test would
/// silently pass.
#[tokio::test]
async fn write_batch_dry_run_rejects_bad_conflict_key_server_side() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_bad_conflict_t (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            )",
        )
        .await
        .expect("create dry_run_bad_conflict_t");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    // `name` is a real column (passes `build_context`'s mapping &
    // describe_schema checks) but has no UNIQUE constraint, so the
    // server rejects `ON CONFLICT (name)` at plan time.
    let spec = WriteSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.dry_run_bad_conflict_t", handle.schema),
        conflict: Some(ConflictConfig {
            key: vec!["name".into()],
            strategy: ConflictStrategy::Overwrite,
        }),
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![CoreRow::upsert(vec![
            Value::Int32(1),
            Value::Text("alice".into()),
        ])],
        next_cursor: None,
    };

    let err = sink
        .write_batch(&spec, &ctx, batch, true)
        .await
        .expect_err("dry-run must reject bad ON CONFLICT key server-side");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("unique") || msg.contains("exclusion") || msg.contains("conflict"),
        "expected pg ON CONFLICT validation error, got: {msg}"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_bad_conflict_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "negative dry-run must not write any rows");

    handle.pool.close().await;
}
