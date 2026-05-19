//! `App::migrate` / `App::validate` / `App::run_once` ordering.
//!
//! Regression for AIR-96: before the fix, `App::migrate` and
//! `App::run_once` both went through a single cached `flows()` helper
//! that ran the full validation pipeline (including sampling-validation)
//! *before* invoking `Storage::migrate`. Sampling-validation drives a
//! dry-run runner tick which loads the column cursor via
//! `Storage::load_cursor` — i.e. `SELECT state FROM air_elt_cursors
//! WHERE flow = $1`. On a fresh database the storage tables don't
//! exist yet, so the operator saw:
//!
//! ```text
//! Error: sampling validation failed for flow "...": ...
//!        relation "air_elt_cursors" does not exist
//! ```
//!
//! The fix splits the cache into two stages: `flows_assembled` (no I/O)
//! and `flows_validated` (I/O probes). `migrate` consumes the assembled
//! stage only, runs `Storage::migrate`, and exits — sampling never fires
//! before the storage tables exist. `run_once` then drives the I/O
//! stage with the storage already migrated.
//!
//! The test uses a Postgres source/sink/storage all pointing at a
//! sandboxed schema. Sampling-validation is opt-in for SQL backends
//! (it's on by default for Mongo), so this test sets `sampling = true`
//! explicitly to reproduce the original bug shape on a real Postgres
//! handle without needing a Mongo container.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use sqlx::Executor;
use sqlx::Row;

/// End-to-end ordering test:
///   1. Build a fresh sandbox schema (no `air_elt_cursors` yet).
///   2. `App::migrate()` must succeed — under the old code, the cached
///      validate pass would have sampled and tripped on the missing
///      cursor table.
///   3. `App::validate()` must succeed afterwards — the storage tables
///      now exist, so sampling-validation can run.
///   4. `App::run_once()` must drain the source into the sink.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn migrate_runs_before_validate_so_sampling_sees_storage_tables() {
    let pg = pg_pool().await;

    // Source + sink tables in the sandbox schema. The source carries a
    // single row so sampling has something to look at and `run_once`
    // has data to drain.
    let src_schema = format!("{}_src", pg.schema);
    let dst_schema = format!("{}_dst", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".items \
                 (id BIGINT PRIMARY KEY, name TEXT NOT NULL)"
            )
            .as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".items \
                 (id BIGINT PRIMARY KEY, name TEXT NOT NULL)"
            )
            .as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "INSERT INTO \"{src_schema}\".items (id, name) VALUES (1, 'alpha'), (2, 'beta')"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // The storage `air_elt_cursors` table does NOT yet exist in this
    // sandbox schema — `pg_pool()` builds a clean schema per test.
    let storage_search_path = format!("{}_storage", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{storage_search_path}\"").as_str())
        .await
        .unwrap();
    let storage_url = {
        let sep = if pg.url.contains('?') { '&' } else { '?' };
        format!(
            "{}{sep}options=-c%20search_path%3D{storage_search_path}",
            pg.url
        )
    };

    // Pre-flight: confirm `air_elt_cursors` does not exist in the
    // storage schema. If this fails the test's invariant is gone.
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = $1 AND table_name = 'air_elt_cursors')",
    )
    .bind(&storage_search_path)
    .fetch_one(&pg.pool)
    .await
    .unwrap();
    assert!(
        !exists,
        "test invariant: air_elt_cursors must not exist before App::migrate"
    );

    let pg_url = pg.url_with_search_path();
    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "snk"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{storage_url}" }}

[flow.items]
source = "src"
sink = "snk"
storage = "st"
from = "{src_schema}.items"
to = "{dst_schema}.items"
batch-limit = 16

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

# Opt sampling-validation on. For SQL backends the factory default is
# off; flipping it on reproduces the original bug shape — the validation
# pipeline drives a dry-run runner tick that reads from
# `air_elt_cursors` before any actual data movement.
validation = {{ sampling = true }}

[flow.items.mapping]
id = "id"
name = "name"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();

    let app = App::from_path(&config_path).expect("App::from_path");

    // 1. `migrate` MUST NOT trigger sampling-validation. Under the old
    //    code this errored with `relation "air_elt_cursors" does not
    //    exist` — the regression we're guarding against.
    app.migrate()
        .await
        .expect("App::migrate on a fresh database");

    // The storage tables must exist after migrate.
    let exists_after: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables \
         WHERE table_schema = $1 AND table_name = 'air_elt_cursors')",
    )
    .bind(&storage_search_path)
    .fetch_one(&pg.pool)
    .await
    .unwrap();
    assert!(exists_after, "App::migrate must create air_elt_cursors");

    // 2. With storage tables present, validation (including sampling)
    //    must succeed.
    app.validate()
        .await
        .expect("App::validate after migrate must succeed");

    // 3. `run_once` drains rows into the sink. Migrate has already run
    //    on this `App`, so the `migrated` flag short-circuits the DDL.
    app.run_once()
        .await
        .expect("App::run_once after migrate must succeed");

    let rows = sqlx::query(&format!(
        "SELECT id, name FROM \"{dst_schema}\".items ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2, "both source rows must reach the sink");
    let id0: i64 = rows[0].try_get("id").unwrap();
    let name0: String = rows[0].try_get("name").unwrap();
    let id1: i64 = rows[1].try_get("id").unwrap();
    let name1: String = rows[1].try_get("name").unwrap();
    assert_eq!((id0, name0.as_str()), (1, "alpha"));
    assert_eq!((id1, name1.as_str()), (2, "beta"));

    pg.pool.close().await;
}
