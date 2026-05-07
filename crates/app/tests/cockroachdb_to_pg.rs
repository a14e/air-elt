//! Reverse path: CockroachDB source → PostgreSQL sink + storage.
//!
//! Validates that the Cockroach-flagged source connector can read through
//! the Postgres wire protocol against a real CockroachDB cluster:
//!   * `validate_access` runs the `has_table_privilege` probe,
//!   * `read_batch` flows through `with_serialization_retry` (a no-op
//!     under no contention but compiles in the Cockroach branch),
//!   * cursor algebra (single-column ASC) hands rows over to the PG
//!     sink unchanged.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cockroachdb_to_pg_smoke() {
    let cockroach = cockroach_pool().await;
    let pg = pg_pool().await;

    let dst_schema = format!("{}_dst", pg.schema);

    cockroach
        .pool
        .execute(
            "CREATE TABLE events (
                id      INT8 PRIMARY KEY,
                payload STRING NOT NULL
            )",
        )
        .await
        .unwrap();
    for i in 1..=4_i64 {
        sqlx::query("INSERT INTO events (id, payload) VALUES ($1, $2)")
            .bind(i)
            .bind(format!("payload-{i}"))
            .execute(&cockroach.pool)
            .await
            .unwrap();
    }

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".events (
                    id      BIGINT PRIMARY KEY,
                    payload TEXT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let cockroach_url = cockroach.url_with_database();
    let pg_url = pg.url_with_search_path();

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "cockroachdb"
config = {{ url = "{cockroach_url}" }}

[[sinks]]
name = "snk"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.events]
source = "src"
sink = "snk"
storage = "st"
from = "public.events"
to = "{dst_schema}.events"
batch-limit = 2

mapping = [
    {{ from = "id", to = "id" }},
    {{ from = "payload", to = "payload" }},
]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, String)> = sqlx::query_as(&format!(
        "SELECT id, payload FROM \"{dst_schema}\".events ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 4);
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.0, (i + 1) as i64);
        assert_eq!(r.1, format!("payload-{}", i + 1));
    }

    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "events");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(4));

    cockroach.pool.close().await;
    pg.pool.close().await;
}
