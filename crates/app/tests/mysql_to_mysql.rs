#![allow(clippy::unwrap_used)]

use air_elt_app::registry::build_registry;
use air_elt_commons_testing::mysql::mysql_pool;
use air_elt_core::config::loader;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::types::Value;
use air_elt_core::validation::pipeline::{assemble, validate};
use chrono::{TimeZone, Utc};
use sqlx::Executor;
use tokio::sync::watch;

/// RAII cleanup for ad-hoc databases created outside the test handle's
/// sandbox. Drop runs DROP on a current-thread runtime in a dedicated thread
/// so it works even when the calling test panics.
struct SiblingDbGuard {
    pool: sqlx::MySqlPool,
    dbs: Vec<String>,
}

impl Drop for SiblingDbGuard {
    fn drop(&mut self) {
        let pool = self.pool.clone();
        let dbs = std::mem::take(&mut self.dbs);
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build cleanup runtime");
            rt.block_on(async move {
                for db in dbs {
                    let stmt = format!("DROP DATABASE IF EXISTS `{db}`");
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        sqlx::query(&stmt).execute(&pool),
                    )
                    .await;
                }
            });
        })
        .join();
    }
}

/// Full MySQL → MySQL pipeline with batch_limit=2 and a tuple cursor over
/// `(created_at, id)`. Exercises:
/// * tinyint(1) → Bool source decoding and sink binding,
/// * varbinary(N) round-trip,
/// * NULL pass-through on a nullable text column,
/// * ON DUPLICATE KEY UPDATE upsert in storage (cursor advances across
///   three batches),
/// * MySQL version probe + dynamic UPSERT dialect selection in storage.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_to_mysql_full_pipeline() {
    let handle = mysql_pool().await;
    let src_db = format!("{}_src", handle.schema);
    let dst_db = format!("{}_dst", handle.schema);

    handle
        .pool
        .execute(format!("CREATE DATABASE `{src_db}`").as_str())
        .await
        .unwrap();
    handle
        .pool
        .execute(format!("CREATE DATABASE `{dst_db}`").as_str())
        .await
        .unwrap();

    // RAII drop guard: ensure sibling databases are removed even on panic.
    // The base sandbox is owned by `MySqlTestHandle`, but these two are
    // outside its scope and would otherwise leak until the 24 h self-heal.
    let _sibling_guard = SiblingDbGuard {
        pool: handle.pool.clone(),
        dbs: vec![src_db.clone(), dst_db.clone()],
    };

    let ddl = |db: &str| {
        format!(
            "CREATE TABLE `{db}`.events (
                id BIGINT NOT NULL,
                created_at TIMESTAMP NOT NULL,
                active TINYINT(1) NOT NULL,
                token VARBINARY(16) NOT NULL,
                description VARCHAR(64),
                PRIMARY KEY (id)
            ) ENGINE=InnoDB"
        )
    };
    handle.pool.execute(ddl(&src_db).as_str()).await.unwrap();
    handle.pool.execute(ddl(&dst_db).as_str()).await.unwrap();

    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    for i in 1..=5_i64 {
        let ts = base + chrono::Duration::seconds(i);
        let active = (i % 2) as i8; // alternate true/false
        let token = vec![i as u8; 16];
        let desc = if i == 3 {
            None
        } else {
            Some(format!("desc-{i}"))
        };
        sqlx::query(&format!(
            "INSERT INTO `{src_db}`.events (id, created_at, active, token, description) \
             VALUES (?, ?, ?, ?, ?)"
        ))
        .bind(i)
        .bind(ts)
        .bind(active)
        .bind(token)
        .bind(desc)
        .execute(&handle.pool)
        .await
        .unwrap();
    }

    let base_url = handle.url_with_database();
    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "mysql"
config = {{ url = "{base_url}" }}

[[sinks]]
name = "snk"
type = "mysql"
config = {{ url = "{base_url}" }}

[[storages]]
name = "st"
type = "mysql"
config = {{ url = "{base_url}" }}

[flow.events]
source = "src"
sink = "snk"
storage = "st"
from = "{src}.events"
to = "{dst}.events"
batch-limit = 2

mapping = [
    {{ from = "id", to = "id" }},
    {{ from = "created_at", to = "created_at" }},
    {{ from = "active", to = "active" }},
    {{ from = "token", to = "token" }},
    {{ from = "description", to = "description" }},
]

cursor = {{ fields = ["created_at", "id"], order = "asc", interval = "100ms" }}
"#,
        base_url = base_url,
        src = src_db,
        dst = dst_db,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, config_toml).unwrap();

    let root = loader::load(&config_path).expect("load config");
    let registry = build_registry();

    let assembled_pre = assemble(&root, &registry).await.expect("assemble");
    let flows_pre = validate(assembled_pre).await.expect("validate");
    for f in &flows_pre {
        f.storage.migrate().await.expect("migrate");
    }
    drop(flows_pre);

    let assembled = assemble(&root, &registry).await.expect("assemble2");
    let flows = validate(assembled).await.expect("validate2");
    let (_tx, rx) = watch::channel(false);
    FlowEngine::new(flows, RunMode::Once, rx)
        .run()
        .await
        .expect("engine run");

    // All 5 rows replicated, NULL preserved for id=3.
    let rows: Vec<(i64, bool, Vec<u8>, Option<String>)> = sqlx::query_as(&format!(
        "SELECT id, active, token, description FROM `{dst_db}`.events ORDER BY id"
    ))
    .fetch_all(&handle.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0].0, 1);
    assert!(rows[0].1, "id=1 odd → active=true");
    assert!(!rows[1].1, "id=2 even → active=false");
    assert_eq!(rows[0].2, vec![1u8; 16]);
    assert!(rows[2].3.is_none(), "NULL description survives pipeline");
    assert_eq!(rows[4].3.as_deref(), Some("desc-5"));

    // Cursor saved as a tuple, advanced to id=5.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&handle.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "events");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields.len(), 2);
    assert_eq!(parsed.fields[1].name, "id");
    assert_eq!(parsed.fields[1].value, Value::Int64(5));
}
