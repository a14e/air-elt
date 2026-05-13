//! Dry-run path for `MySqlSink::write_batch` (T6).
//!
//! `dry_run = true` must build the same SQL as production (planner
//! parses every bind, types are checked, server-side column
//! constraints fire) but a `tx.begin() → tx.rollback()` envelope
//! keeps the table empty and forces `rows_written = 0`.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mysql::mysql_pool;
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_mysql::{MySqlSink, MySqlSinkConfig};
use sqlx::Executor;

#[tokio::test]
async fn write_batch_dry_run_skips_writes_then_real_run_commits() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_t (
                id INT NOT NULL PRIMARY KEY,
                name VARCHAR(64) NOT NULL
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create dry_run_t");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
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

    let report_dry = sink
        .write_batch(&spec, &ctx, make_batch(), true)
        .await
        .expect("dry-run write");
    assert_eq!(report_dry.rows_written, 0);
    let count_after_dry: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(
        count_after_dry, 0,
        "dry-run must leave the target table empty"
    );

    let report = sink
        .write_batch(&spec, &ctx, make_batch(), false)
        .await
        .expect("real write");
    assert_eq!(report.rows_written, 2);
    let count_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_after, 2);

    handle.pool.close().await;
}

/// Negative case: a `Value::Text` carrying malformed JSON bound
/// against a `JSON NOT NULL` column. The previous `WHERE FALSE`
/// shape would have short-circuited the projection and let the bind
/// through silently; the `tx.begin()/INSERT/rollback` shape forces
/// MySQL to parse the JSON server-side and surface the failure.
#[tokio::test]
async fn write_batch_dry_run_rejects_invalid_json_server_side() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_neg_json_t (
                id INT NOT NULL PRIMARY KEY,
                payload JSON NOT NULL
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create dry_run_neg_json_t");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "payload".into()],
        table: format!("{}.dry_run_neg_json_t", handle.schema),
        conflict: None,
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // `Value::Text("{not json")` against the JSON column. The bind
    // travels as a string and MySQL parses it as JSON before the
    // INSERT lands — malformed JSON triggers a server error.
    let bad_batch = Batch {
        rows: vec![CoreRow::upsert(vec![
            Value::Int32(1),
            Value::Text("{not json".into()),
        ])],
        next_cursor: None,
    };

    let err = sink
        .write_batch(&spec, &ctx, bad_batch, true)
        .await
        .expect_err("dry-run must reject malformed JSON server-side");
    let msg = format!("{err:#}").to_lowercase();
    assert!(
        msg.contains("json") || msg.contains("invalid") || msg.contains("incorrect"),
        "expected mysql JSON-parse error, got: {msg}"
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_neg_json_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count, 0, "negative dry-run must not write any rows");

    handle.pool.close().await;
}

/// Delete dry-run against seeded rows: proves the dry-run delete path
/// is a true no-op against pre-existing data, and that the same batch
/// in production mode actually removes the rows. Closes the gap where
/// the existing delete dry-run test only ran against an empty target
/// (where `Ok(0)` short-circuit would also have passed).
#[tokio::test]
async fn write_batch_dry_run_delete_preserves_then_real_run_deletes() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE dry_run_del_seeded_t (
                id INT NOT NULL PRIMARY KEY,
                name VARCHAR(64) NOT NULL
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create dry_run_del_seeded_t");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
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
    assert_eq!(report_dry.rows_written, 0);
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
    assert_eq!(report_real.rows_written, 2);
    let count_after_real: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dry_run_del_seeded_t")
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count_after_real, 0, "real delete must remove seeded rows");

    handle.pool.close().await;
}
