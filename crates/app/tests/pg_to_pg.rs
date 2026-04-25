#![allow(clippy::unwrap_used)]

use air_elt_app::registry::build_registry;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::config::loader;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::types::Value;
use air_elt_core::validation::pipeline::{assemble, validate};
use chrono::{NaiveDate, TimeZone, Utc};
use sqlx::Executor;
use tokio::sync::watch;
use uuid::Uuid;

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
batch-limit = 2

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
    let flows_pre = assemble(&root, &registry)
        .await
        .expect("pre-migrate assemble");
    validate(&flows_pre).await.expect("pre-migrate validate");
    for f in &flows_pre {
        f.storage.migrate().await.expect("migrate");
    }
    drop(flows_pre);

    // Re-assemble + validate so the sink's access probe runs against the migrated storage.
    let flows = assemble(&root, &registry).await.expect("assemble");
    validate(&flows).await.expect("validate");
    let (_tx, rx) = watch::channel(false);
    FlowEngine::new(flows, RunMode::Once, rx)
        .run()
        .await
        .expect("engine run");

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

    let parsed: air_elt_core::model::CursorState =
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

/// All 12 DataType variants, nullable + non-nullable, multi-batch with
/// batch-limit=2 and 4 rows (2 batches). Validates full round-trip of
/// every canonical type through source → sink pipeline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_types_postgres_to_postgres() {
    let handle = pg_pool().await;
    let src_schema = format!("{}_allsrc", handle.schema);
    let dst_schema = format!("{}_alldst", handle.schema);

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
            "CREATE TABLE \"{schema}\".all_types (
                id BIGINT NOT NULL PRIMARY KEY,
                c_bool BOOLEAN NOT NULL,
                c_i16 SMALLINT NOT NULL,
                c_i32 INTEGER NOT NULL,
                c_i64 BIGINT NOT NULL,
                c_f32 REAL NOT NULL,
                c_f64 DOUBLE PRECISION NOT NULL,
                c_text TEXT NOT NULL,
                c_bytes BYTEA NOT NULL,
                c_date DATE NOT NULL,
                c_ts TIMESTAMPTZ NOT NULL,
                c_uuid UUID NOT NULL,
                c_json JSONB NOT NULL,
                n_bool BOOLEAN,
                n_text TEXT,
                n_uuid UUID,
                n_json JSONB
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

    let ts1 = Utc.with_ymd_and_hms(2026, 1, 15, 8, 30, 0).unwrap();
    let ts2 = Utc.with_ymd_and_hms(2026, 6, 20, 14, 0, 0).unwrap();
    let d1 = NaiveDate::from_ymd_opt(2026, 3, 1).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2026, 12, 31).unwrap();
    let u1 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440001").unwrap();
    let u2 = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440002").unwrap();

    let insert = format!(
        "INSERT INTO \"{src_schema}\".all_types \
         (id, c_bool, c_i16, c_i32, c_i64, c_f32, c_f64, c_text, c_bytes, c_date, c_ts, c_uuid, c_json, \
          n_bool, n_text, n_uuid, n_json) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)"
    );

    // Row 1: all non-null
    sqlx::query(&insert)
        .bind(1_i64)
        .bind(true)
        .bind(42_i16)
        .bind(1000_i32)
        .bind(999_999_i64)
        .bind(std::f32::consts::PI)
        .bind(std::f64::consts::E)
        .bind("hello")
        .bind(vec![0xDE_u8, 0xAD, 0xBE, 0xEF])
        .bind(d1)
        .bind(ts1)
        .bind(u1)
        .bind(serde_json::json!({"key": "val1"}))
        .bind(Some(true))
        .bind(Some("nullable-text"))
        .bind(Some(u1))
        .bind(Some(serde_json::json!({"n": 1})))
        .execute(&handle.pool)
        .await
        .unwrap();

    // Row 2: nullable fields are NULL
    sqlx::query(&insert)
        .bind(2_i64)
        .bind(false)
        .bind(-1_i16)
        .bind(-500_i32)
        .bind(i64::MAX)
        .bind(0.0_f32)
        .bind(f64::MIN_POSITIVE)
        .bind("")
        .bind(Vec::<u8>::new())
        .bind(d2)
        .bind(ts2)
        .bind(u2)
        .bind(serde_json::json!([1, 2, 3]))
        .bind(Option::<bool>::None)
        .bind(Option::<String>::None)
        .bind(Option::<Uuid>::None)
        .bind(Option::<serde_json::Value>::None)
        .execute(&handle.pool)
        .await
        .unwrap();

    // Row 3: mixed nulls
    sqlx::query(&insert)
        .bind(3_i64)
        .bind(true)
        .bind(0_i16)
        .bind(0_i32)
        .bind(0_i64)
        .bind(-0.0_f32)
        .bind(0.0_f64)
        .bind("z")
        .bind(vec![0_u8])
        .bind(d1)
        .bind(ts1)
        .bind(u1)
        .bind(serde_json::json!(null))
        .bind(Some(false))
        .bind(Option::<String>::None)
        .bind(Some(u2))
        .bind(Option::<serde_json::Value>::None)
        .execute(&handle.pool)
        .await
        .unwrap();

    // Row 4: all nullable filled
    sqlx::query(&insert)
        .bind(4_i64)
        .bind(false)
        .bind(i16::MAX)
        .bind(i32::MAX)
        .bind(i64::MIN)
        .bind(f32::MAX)
        .bind(f64::MAX)
        .bind("long-text-value")
        .bind(vec![255_u8; 100])
        .bind(d2)
        .bind(ts2)
        .bind(u2)
        .bind(serde_json::json!({"nested": {"deep": true}}))
        .bind(Some(true))
        .bind(Some("four"))
        .bind(Some(u2))
        .bind(Some(serde_json::json!(42)))
        .execute(&handle.pool)
        .await
        .unwrap();

    let base_url = handle.url_with_search_path();
    let columns = [
        "id", "c_bool", "c_i16", "c_i32", "c_i64", "c_f32", "c_f64", "c_text", "c_bytes", "c_date",
        "c_ts", "c_uuid", "c_json", "n_bool", "n_text", "n_uuid", "n_json",
    ];
    let mapping_toml: String = columns
        .iter()
        .map(|c| format!("    {{ from = \"{c}\", to = \"{c}\" }}"))
        .collect::<Vec<_>>()
        .join(",\n");

    let config_toml = format!(
        r#"
[[sources]]
name = "s"
type = "postgres"
config = {{ url = "{base_url}" }}

[[sinks]]
name = "k"
type = "postgres"
config = {{ url = "{base_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{base_url}" }}

[flow.all_types]
source = "s"
sink = "k"
storage = "st"
from = "{src}.all_types"
to = "{dst}.all_types"
batch-limit = 2

mapping = [
{mapping}
]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
        src = src_schema,
        dst = dst_schema,
        mapping = mapping_toml,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();

    let root = loader::load(&config_path).expect("load");
    let registry = build_registry();

    let flows_pre = assemble(&root, &registry).await.expect("assemble");
    validate(&flows_pre).await.expect("validate");
    for f in &flows_pre {
        f.storage.migrate().await.expect("migrate");
    }
    drop(flows_pre);

    let flows = assemble(&root, &registry).await.expect("assemble2");
    validate(&flows).await.expect("validate2");
    let (_tx, rx) = watch::channel(false);
    FlowEngine::new(flows, RunMode::Once, rx)
        .run()
        .await
        .expect("engine run");

    // Verify all 4 rows arrived (2 batches of 2).
    let count: (i64,) = sqlx::query_as(&format!("SELECT count(*) FROM \"{dst_schema}\".all_types"))
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert_eq!(count.0, 4, "all 4 rows should be in sink");

    // Spot-check row 1 non-null types.
    let (c_bool, c_i16, c_text, c_uuid): (bool, i16, String, Uuid) = sqlx::query_as(&format!(
        "SELECT c_bool, c_i16, c_text, c_uuid FROM \"{dst_schema}\".all_types WHERE id = 1"
    ))
    .fetch_one(&handle.pool)
    .await
    .unwrap();
    assert!(c_bool);
    assert_eq!(c_i16, 42);
    assert_eq!(c_text, "hello");
    assert_eq!(c_uuid, u1);

    // Spot-check row 2 nullable NULLs.
    let (n_bool, n_text, n_uuid): (Option<bool>, Option<String>, Option<Uuid>) = sqlx::query_as(
        &format!("SELECT n_bool, n_text, n_uuid FROM \"{dst_schema}\".all_types WHERE id = 2"),
    )
    .fetch_one(&handle.pool)
    .await
    .unwrap();
    assert!(n_bool.is_none());
    assert!(n_text.is_none());
    assert!(n_uuid.is_none());

    // Spot-check row 4 bytes + json.
    let (c_bytes, c_json): (Vec<u8>, serde_json::Value) = sqlx::query_as(&format!(
        "SELECT c_bytes, c_json FROM \"{dst_schema}\".all_types WHERE id = 4"
    ))
    .fetch_one(&handle.pool)
    .await
    .unwrap();
    assert_eq!(c_bytes.len(), 100);
    assert_eq!(c_json, serde_json::json!({"nested": {"deep": true}}));

    // Verify row 2 extreme values survive the round-trip without corruption.
    // Row 2 uses: i64::MAX, 0.0_f32 (zero-float binding), empty string, empty bytes,
    // d2 (2026-12-31), ts2, u2, a JSON array, and all nullable columns NULL.
    #[allow(clippy::type_complexity)]
    let (c_bool2, c_i16_2, c_i32_2, c_i64_2, c_f32_2, c_f64_2, c_text2, c_bytes2, c_date2, c_ts2, c_uuid2, c_json2):
        (bool, i16, i32, i64, f32, f64, String, Vec<u8>, NaiveDate, chrono::DateTime<chrono::Utc>, Uuid, serde_json::Value) =
        sqlx::query_as(&format!(
            "SELECT c_bool, c_i16, c_i32, c_i64, c_f32, c_f64, c_text, c_bytes, c_date, c_ts, c_uuid, c_json \
             FROM \"{dst_schema}\".all_types WHERE id = 2"
        ))
        .fetch_one(&handle.pool)
        .await
        .unwrap();
    assert!(!c_bool2, "row2 c_bool should be false");
    assert_eq!(c_i16_2, -1_i16, "row2 c_i16");
    assert_eq!(c_i32_2, -500_i32, "row2 c_i32");
    assert_eq!(c_i64_2, i64::MAX, "row2 c_i64 must survive as i64::MAX");
    assert_eq!(c_f32_2, 0.0_f32, "row2 c_f32 zero must not be dropped");
    assert_eq!(c_f64_2, f64::MIN_POSITIVE, "row2 c_f64");
    assert_eq!(c_text2, "", "row2 c_text should be empty string");
    assert!(c_bytes2.is_empty(), "row2 c_bytes should be empty");
    assert_eq!(c_date2, d2, "row2 c_date should match d2 (2026-12-31)");
    assert_eq!(c_ts2, ts2, "row2 c_ts should match ts2");
    assert_eq!(c_uuid2, u2, "row2 c_uuid should match u2");
    assert_eq!(c_json2, serde_json::json!([1, 2, 3]), "row2 c_json array");
}
