//! Same-vendor: PostgreSQL source → PostgreSQL sink, PostgreSQL storage.
//!
//! Two e2e cases for wildcard + JSON auto-pack:
//!   * `pg_to_pg_wildcard_round_trip` — `mapping = ["*"]` against a 3-col
//!     table; round-trips every column (including a NULL) into a sink
//!     table with the same schema.
//!   * `pg_to_pg_json_auto_pack` — `mapping = ["id", "*:body"]`; sink has
//!     `(id, body JSONB)` and the runner packs every source field into
//!     `body`: `numeric` → JSON string (Decimal),
//!     plain ints stay JSON numbers, text stays a string.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_pg_wildcard_round_trip() {
    let pg = pg_pool().await;

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
                "CREATE TABLE \"{src_schema}\".people (
                    id   BIGINT NOT NULL,
                    name TEXT,
                    age  INT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Sink table mirrors source. Wildcard expansion picks the sink schema
    // first; same-name columns flow straight through.
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".people (
                    id   BIGINT NOT NULL,
                    name TEXT,
                    age  INT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let insert = format!("INSERT INTO \"{src_schema}\".people (id, name, age) VALUES ($1, $2, $3)");
    // Three rows, including one with NULL `name` (row id=2).
    let fixtures: [(i64, Option<&str>, i32); 3] = [
        (1, Some("alice"), 30),
        (2, None, 41),
        (3, Some("carol"), 27),
    ];
    for (id, name, age) in fixtures {
        sqlx::query(&insert)
            .bind(id)
            .bind(name)
            .bind(age)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

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
config = {{ url = "{pg_url}" }}

[flow.people]
source = "src"
sink = "snk"
storage = "st"
from = "{src_schema}.people"
to = "{dst_schema}.people"
batch-limit = 8

mapping = ["*"]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, Option<String>, i32)> = sqlx::query_as(&format!(
        "SELECT id, name, age FROM \"{dst_schema}\".people ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3, "all source rows must reach the sink");
    assert_eq!(rows[0], (1, Some("alice".to_string()), 30));
    assert_eq!(
        rows[1],
        (2, None, 41),
        "NULL name must round-trip as NULL, not be dropped or coerced"
    );
    assert_eq!(rows[2], (3, Some("carol".to_string()), 27));

    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_pg_json_auto_pack() {
    let pg = pg_pool().await;

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
                "CREATE TABLE \"{src_schema}\".events (
                    id    BIGINT NOT NULL,
                    name  TEXT NOT NULL,
                    score NUMERIC(10,4) NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".events (
                    id   BIGINT,
                    body JSONB
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Three rows with distinct shapes.
    let inserts = [
        (1_i64, "alpha", "12.3400"),
        (2, "beta", "0.5000"),
        (3, "gamma", "999.9999"),
    ];
    let insert = format!(
        "INSERT INTO \"{src_schema}\".events (id, name, score) \
         VALUES ($1, $2, $3::numeric)"
    );
    for (id, name, score) in inserts {
        sqlx::query(&insert)
            .bind(id)
            .bind(name)
            .bind(score)
            .execute(&pg.pool)
            .await
            .unwrap();
    }

    let pg_url = pg.url_with_search_path();

    // Mapping: `id` direct + `*:body` packs every source field (id, name,
    // score) into `body`. Cursor on `id` (an explicit Direct entry — must
    // exist post-expansion).
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
config = {{ url = "{pg_url}" }}

[flow.events]
source = "src"
sink = "snk"
storage = "st"
from = "{src_schema}.events"
to = "{dst_schema}.events"
batch-limit = 8

mapping = ["id", "*:body"]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, serde_json::Value)> = sqlx::query_as(&format!(
        "SELECT id, body FROM \"{dst_schema}\".events ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3, "all rows must land in the sink");

    // JSON encoding rules:
    //   - integers stay JSON numbers,
    //   - text stays a JSON string,
    //   - Decimal serialises as a JSON string (lossless),
    //   - the packed object includes every source column under its own name.
    let expected: [(i64, serde_json::Value); 3] = [
        (
            1,
            serde_json::json!({ "id": 1, "name": "alpha", "score": "12.3400" }),
        ),
        (
            2,
            serde_json::json!({ "id": 2, "name": "beta", "score": "0.5000" }),
        ),
        (
            3,
            serde_json::json!({ "id": 3, "name": "gamma", "score": "999.9999" }),
        ),
    ];

    for (i, (id, body)) in rows.iter().enumerate() {
        let (expected_id, ref expected_body) = expected[i];
        assert_eq!(*id, expected_id, "row {i}: id column");
        assert_eq!(
            body, expected_body,
            "row {i}: packed body must contain every source field"
        );
    }

    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_pg_with_mssql_storage() {
    let pg = pg_pool().await;
    let ms = air_elt_commons_testing::mssql::mssql_pool().await;

    let src_table = format!("{}.items", pg.schema);
    let dst_table = format!("{}.items_dst", pg.schema);

    pg.pool
        .execute(
            format!("CREATE TABLE {src_table} (id BIGINT NOT NULL, val INT NOT NULL)").as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!("CREATE TABLE {dst_table} (id BIGINT NOT NULL, val INT NOT NULL)").as_str(),
        )
        .await
        .unwrap();

    sqlx::query(&format!(
        "INSERT INTO {src_table} (id, val) VALUES ($1, $2)"
    ))
    .bind(1i64)
    .bind(100i32)
    .execute(&pg.pool)
    .await
    .unwrap();

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
type = "mssql"
config = {{ url = "{ms_url}" }}

[flow.items]
source = "src"
sink = "snk"
storage = "st"
from = "{src_table}"
to = "{dst_table}"
batch-limit = 8

mapping = ["*"]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
        pg_url = pg.url_with_search_path(),
        ms_url = ms.url,
        src_table = src_table,
        dst_table = dst_table,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let rows: Vec<(i64, i32)> =
        sqlx::query_as(&format!("SELECT id, val FROM {dst_table} ORDER BY id"))
            .fetch_all(&pg.pool)
            .await
            .unwrap();

    assert_eq!(rows.len(), 1, "row must reach sink with mssql storage");
    assert_eq!(rows[0], (1, 100));

    pg.pool.close().await;
    drop(ms);
}
