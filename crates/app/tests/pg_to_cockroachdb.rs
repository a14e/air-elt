//! Cross-engine: PostgreSQL source → CockroachDB sink, CockroachDB storage.
//!
//! Exercises the alias-on-postgres + `Dialect::Cockroach` path end-to-end
//! through the CLI/engine code path:
//!   * the registry resolves `type = "cockroachdb"` to the same `PgSink`/
//!     `PgStorage` types but with `Dialect::Cockroach`,
//!   * `Storage::migrate` finds and applies `migrations/storage-cockroachdb/`
//!     (with advisory-lock disabled — Cockroach has no `pg_advisory_lock`),
//!   * cursor save/load round-trips through CockroachDB's JSONB,
//!   * `INSERT … ON CONFLICT (id) DO UPDATE` runs against Cockroach with
//!     the same semantics it has on Postgres — overwrite of a pre-existing
//!     row succeeds.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_cockroachdb_with_upsert_overwrite() {
    let pg = pg_pool().await;
    let cockroach = cockroach_pool().await;

    let src_schema = format!("{}_src", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".users (
                    id            BIGINT PRIMARY KEY,
                    email         TEXT NOT NULL,
                    display_name  TEXT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Cockroach sandbox database is dropped by the handle's Drop.
    cockroach
        .pool
        .execute(
            "CREATE TABLE users (
                id            INT8 PRIMARY KEY,
                email         STRING NOT NULL,
                display_name  STRING NOT NULL
            )",
        )
        .await
        .unwrap();
    // Pre-existing row that the sink must overwrite via UPSERT.
    cockroach
        .pool
        .execute("INSERT INTO users (id, email, display_name) VALUES (1, 'old@x', 'stale')")
        .await
        .unwrap();

    for i in 1..=5_i64 {
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".users (id, email, display_name) VALUES ($1, $2, $3)"
        ))
        .bind(i)
        .bind(format!("user{i}@example.com"))
        .bind(format!("User {i}"))
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let cockroach_url = cockroach.url_with_database();

    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: cockroachdb
    config:
      url: "{cockroach_url}"

storages:
  - name: st
    type: cockroachdb
    config:
      url: "{cockroach_url}"

flow:
  users:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.users"
    to: "public.users"
    batch-limit: 2

    mapping:
      - {{ from: id, to: id }}
      - {{ from: email, to: email }}
      - {{ from: display_name, to: display_name }}

    cursor:
      fields: [id]
      order: asc
      interval: "100ms"

    # Single-key Overwrite -- drives the sink down the UPSERT path on Cockroach.
    conflict:
      key: [id]
      strategy: overwrite
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // 5 rows landed; row id=1 was overwritten (stale → fresh email).
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, email, display_name FROM users ORDER BY id")
            .fetch_all(&cockroach.pool)
            .await
            .unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].0, 1);
    assert_eq!(rows[0].1, "user1@example.com", "UPSERT overwrote stale row");
    assert_eq!(rows[0].2, "User 1");
    for (i, r) in rows.iter().enumerate() {
        let expected = (i + 1) as i64;
        assert_eq!(r.0, expected);
    }

    // Cursor saved into the cockroach state table via `Storage::save_cursor`,
    // which goes through `with_serialization_retry` under Cockroach dialect.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&cockroach.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "users");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(5));

    pg.pool.close().await;
    cockroach.pool.close().await;
}
