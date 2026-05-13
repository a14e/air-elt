//! Cross-vendor: PostgreSQL source → MS SQL sink.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mssql::mssql_pool;
use air_elt_commons_testing::pg::pg_pool;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_mssql_wildcard_round_trip() {
    let pg = pg_pool().await;
    let ms = mssql_pool().await;

    let src_table = format!("{}.people", pg.schema);
    let dst_table = format!("[{db}].dbo.people", db = ms.database);

    pg.pool
        .execute(
            format!(
                "CREATE TABLE {src_table} ( \
                    id   BIGINT NOT NULL, \
                    name TEXT, \
                    age  INT NOT NULL \
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let mut conn = ms.pool.get().await.unwrap();
    conn.simple_query(&format!(
        "CREATE TABLE {dst_table} ( \
            id   BIGINT NOT NULL, \
            name NVARCHAR(100), \
            age  INT NOT NULL \
        )",
    ))
    .await
    .unwrap();
    drop(conn);

    for (id, name, age) in [
        (1i64, Some("alice"), 30i32),
        (2, None, 41),
        (3, Some("carol"), 27),
    ] {
        sqlx::query(&format!(
            "INSERT INTO {src_table} (id, name, age) VALUES ($1, $2, $3)"
        ))
        .bind(id)
        .bind(name)
        .bind(age)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "snk"
type = "mssql"
config = {{ url = "{ms_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.people]
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

    let mut conn = ms.pool.get().await.unwrap();
    let stream = conn
        .simple_query(&format!(
            "SELECT id, name, age FROM {dst_table} ORDER BY id"
        ))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    assert_eq!(rows.len(), 3, "all PG rows must reach MS SQL sink");

    let r0_id: i64 = rows[0].try_get::<i64, _>(0).unwrap().unwrap();
    let r0_name: &str = rows[0].try_get::<&str, _>(1).unwrap().unwrap();
    let r0_age: i32 = rows[0].try_get::<i32, _>(2).unwrap().unwrap();
    assert_eq!((r0_id, r0_name, r0_age), (1, "alice", 30));

    let r1_id: i64 = rows[1].try_get::<i64, _>(0).unwrap().unwrap();
    let r1_name: Option<&str> = rows[1].try_get::<&str, _>(1).unwrap();
    let r1_age: i32 = rows[1].try_get::<i32, _>(2).unwrap().unwrap();
    assert_eq!(r1_id, 2);
    assert_eq!(r1_name, None, "NULL must cross vendor boundary");
    assert_eq!(r1_age, 41);

    let r2_id: i64 = rows[2].try_get::<i64, _>(0).unwrap().unwrap();
    let r2_name: &str = rows[2].try_get::<&str, _>(1).unwrap().unwrap();
    let r2_age: i32 = rows[2].try_get::<i32, _>(2).unwrap().unwrap();
    assert_eq!((r2_id, r2_name, r2_age), (3, "carol", 27));
    drop(conn);

    pg.pool.close().await;
    drop(ms);
}
