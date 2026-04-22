use std::sync::Arc;

use air_elt_app::registry::build_registry;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::config::loader;
use air_elt_core::flow::runner::{RunMode, run_all_flows};
use air_elt_core::types::Value;
use air_elt_core::validation::pipeline::validate;
use chrono::{TimeZone, Utc};
use sqlx::Executor;
use tokio::sync::watch;

/// Why nullable + batch_limit=2 + tuple cursor: the earlier version of this
/// test was degenerate — all columns NOT NULL, batch_limit > row count, and a
/// single-column integer cursor. That shape never exercised multi-batch drain,
/// nullable NULL handling, or the tuple-cursor lex-compare. This test now
/// covers all three in one run.
async fn prepare_schemas(handle: &air_elt_commons_testing::pg::PgTestHandle) -> (String, String) {
    let src_schema = format!("{}_src", handle.schema);
    let dst_schema = format!("{}_dst", handle.schema);

    handle
        .pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    handle
        .pool
        .execute(format!("CREATE SCHEMA \"{dst_schema}\"").as_str())
        .await
        .unwrap();

    let ddl = |schema: &str| {
        format!(
            "CREATE TABLE \"{schema}\".users (
                id BIGINT NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                name TEXT NOT NULL,
                description TEXT,
                PRIMARY KEY (id)
            )"
        )
    };
    handle
        .pool
        .execute(ddl(&src_schema).as_str())
        .await
        .unwrap();
    handle
        .pool
        .execute(ddl(&dst_schema).as_str())
        .await
        .unwrap();

    // Monotone (created_at, id) tuple with one NULL description so the sink's
    // NULL-binding path runs inside the pipeline.
    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    for i in 1..=5_i64 {
        let ts = base + chrono::Duration::seconds(i);
        let desc = if i == 3 {
            None
        } else {
            Some(format!("desc-{i}"))
        };
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".users (id, created_at, name, description) \
             VALUES ($1, $2, $3, $4)"
        ))
        .bind(i)
        .bind(ts)
        .bind(format!("user-{i}"))
        .bind(desc)
        .execute(&handle.pool)
        .await
        .unwrap();
    }

    (src_schema, dst_schema)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_postgres_to_postgres_once() {
    let handle = pg_pool().await;
    let (src_schema, dst_schema) = prepare_schemas(&handle).await;
    let base_url = handle.url_with_search_path();

    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{base_url}" }}

[[sinks]]
name = "pg_sink"
type = "postgres"
config = {{ url = "{base_url}" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{base_url}" }}

[flow.users]
source = "pg_src"
sink = "pg_sink"
storage = "pg_state"
from = "{src}.users"
to = "{dst}.users"
batch_limit = 2

mapping = [
    {{ from = "id", to = "id" }},
    {{ from = "created_at", to = "created_at" }},
    {{ from = "name", to = "name" }},
    {{ from = "description", to = "description" }},
]

cursor = {{ fields = ["created_at", "id"], order = "asc", interval = "200ms" }}
"#,
        base_url = base_url,
        src = src_schema,
        dst = dst_schema,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, config_toml).unwrap();

    let root = loader::load(&config_path).expect("load config");
    let registry = build_registry();

    // Migrate storage first
    let flows_pre = validate(&root, &registry)
        .await
        .expect("pre-migrate validate");
    for f in &flows_pre {
        f.storage.migrate().await.expect("migrate");
    }

    // Re-validate so the sink's access probe runs against the migrated storage.
    let flows = validate(&root, &registry).await.expect("validate");
    let flows: Vec<_> = flows.into_iter().map(Arc::new).collect();
    let (_tx, rx) = watch::channel(false);
    run_all_flows(flows, RunMode::Once, rx)
        .await
        .expect("run_all_flows");

    // Verify sink received all 5 rows (3 batches: 2 + 2 + 1).
    let rows: Vec<(i64, Option<String>)> = sqlx::query_as(&format!(
        "SELECT id, description FROM \"{dst_schema}\".users ORDER BY id"
    ))
    .fetch_all(&handle.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);
    // description NULL round-trips for id=3.
    assert_eq!(rows[2].0, 3);
    assert!(
        rows[2].1.is_none(),
        "NULL description must survive the pipeline"
    );
    assert_eq!(rows[4].1.as_deref(), Some("desc-5"));

    // Verify cursor was saved with both fields and advances as a tuple.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&handle.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "users");

    let parsed: air_elt_core::flow::state::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(
        parsed.fields.len(),
        2,
        "tuple cursor must carry both fields"
    );
    assert_eq!(parsed.fields[0].name, "created_at");
    assert_eq!(parsed.fields[1].name, "id");
    assert_eq!(parsed.fields[1].value, Value::Int64(5));
}
