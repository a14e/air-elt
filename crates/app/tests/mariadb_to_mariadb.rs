//! Full pipeline against **MariaDB**: registry uses the same `mysql` factory,
//! but the live server diverges from MySQL on UPSERT syntax (legacy
//! `VALUES()` form) and on a native `UUID` column type. This test exercises
//! both — they're the only divergences worth a dedicated cross-vendor flow,
//! per the no-N×N testing convention.
#![allow(clippy::unwrap_used)]

use air_elt_app::registry::build_registry;
use air_elt_commons_testing::mariadb::mariadb_pool;
use air_elt_core::config::loader;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::types::Value;
use air_elt_core::validation::pipeline::{assemble, validate};
use sqlx::Executor;
use tokio::sync::watch;
use uuid::Uuid;

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mariadb_to_mariadb_with_native_uuid() {
    let handle = mariadb_pool().await;
    let src_db = format!("{}_src", handle.schema);
    let dst_db = format!("{}_dst", handle.schema);

    // Register the cleanup guard *before* CREATE so a panic between the two
    // CREATEs (or any later step) still drops both databases. DROP IF EXISTS
    // is a no-op for the not-yet-created one — safe.
    let _sibling_guard = SiblingDbGuard {
        pool: handle.pool.clone(),
        dbs: vec![src_db.clone(), dst_db.clone()],
    };

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

    let ddl = |db: &str| {
        format!(
            "CREATE TABLE `{db}`.accounts (
                id BIGINT NOT NULL PRIMARY KEY,
                ext UUID NOT NULL
            ) ENGINE=InnoDB"
        )
    };
    handle.pool.execute(ddl(&src_db).as_str()).await.unwrap();
    handle.pool.execute(ddl(&dst_db).as_str()).await.unwrap();

    // Well-formed RFC 4122 UUIDs (version 4 / variant 8) — MariaDB UUID
    // columns reject invalid version/variant nibbles.
    let uuids: Vec<Uuid> = (0..3u128)
        .map(|i| Uuid::from_u128(0x1000_0000_0000_4000_8000_0000_0000_0000_u128 + i))
        .collect();
    for (i, u) in uuids.iter().enumerate() {
        sqlx::query(&format!(
            "INSERT INTO `{src_db}`.accounts (id, ext) VALUES (?, ?)"
        ))
        .bind((i + 1) as i64)
        // Bind as canonical text — see sink codec note on MariaDB's UUID
        // byte-shuffle.
        .bind(u.to_string())
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

[flow.accounts]
source = "src"
sink = "snk"
storage = "st"
from = "{src}.accounts"
to = "{dst}.accounts"
batch-limit = 2

mapping = [
    {{ from = "id", to = "id" }},
    {{ from = "ext", to = "ext" }},
]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
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

    let rows: Vec<(i64, Vec<u8>)> = sqlx::query_as(&format!(
        "SELECT id, ext FROM `{dst_db}`.accounts ORDER BY id"
    ))
    .fetch_all(&handle.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);
    for (i, (id, ext)) in rows.iter().enumerate() {
        assert_eq!(*id, (i + 1) as i64);
        let text = std::str::from_utf8(ext).expect("uuid text");
        let got = Uuid::parse_str(text).expect("parse");
        assert_eq!(got, uuids[i]);
    }

    // Cursor advanced and stored via the legacy `VALUES()` UPSERT path.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&handle.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(3));
}
