//! Cross-vendor: MySQL source → MariaDB sink, MariaDB storage.
//!
//! Both wear the `type = "mysql"` connector hat — only the live server
//! diverges. This pipeline proves:
//!   * the runner happily talks to two physically distinct MySQL-protocol
//!     servers in the same flow,
//!   * `Text(36) → Uuid` runtime conversion (`convert::uuid::parse_text`)
//!     bridges a `VARCHAR(36)` column on the MySQL side to a native
//!     `UUID` column on MariaDB 10.7+,
//!   * mixed nullable + NOT NULL columns survive the round-trip, with
//!     NULL values preserved on the nullable ones,
//!   * a NOT NULL sink column paired with a nullable source column is
//!     bridged via `default = "..."` on the mapping, and the runtime
//!     substitution actually fires on rows where the source was NULL,
//!   * MariaDB storage uses the legacy `VALUES()` UPSERT dialect for
//!     `air_elt_cursors`.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mariadb::mariadb_pool;
use air_elt_commons_testing::mysql::mysql_pool;
use air_elt_core::types::Value;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_to_mariadb_with_uuid_and_mixed_nullability() {
    let mysql = mysql_pool().await;
    let mariadb = mariadb_pool().await;

    let src_db = format!("{}_src", mysql.schema);
    let dst_db = format!("{}_dst", mariadb.schema);

    mysql
        .pool
        .execute(format!("CREATE DATABASE `{src_db}`").as_str())
        .await
        .unwrap();
    // Source mixes nullable and NOT NULL columns, plus a `VARCHAR(36)`
    // that will be reinterpreted as a MariaDB native `UUID` on the sink.
    mysql
        .pool
        .execute(
            format!(
                "CREATE TABLE `{src_db}`.accounts (
                    id BIGINT NOT NULL PRIMARY KEY,
                    ext VARCHAR(36) NOT NULL,
                    label VARCHAR(64) NOT NULL,
                    note VARCHAR(64),
                    score INT,
                    active TINYINT(1) NOT NULL,
                    last_seen TIMESTAMP NULL
                ) ENGINE=InnoDB"
            )
            .as_str(),
        )
        .await
        .unwrap();

    mariadb
        .pool
        .execute(format!("CREATE DATABASE `{dst_db}`").as_str())
        .await
        .unwrap();
    // Sink keeps the same NOT NULL distribution as the source for the
    // mostly-NOT-NULL columns, then introduces a NOT NULL `note_safe`
    // backed by a default to verify nullable_src → not_null_sink works.
    mariadb
        .pool
        .execute(
            format!(
                "CREATE TABLE `{dst_db}`.accounts (
                    id BIGINT NOT NULL PRIMARY KEY,
                    ext UUID NOT NULL,
                    label VARCHAR(64) NOT NULL,
                    note_safe VARCHAR(64) NOT NULL,
                    score INT,
                    active TINYINT(1) NOT NULL,
                    last_seen TIMESTAMP NULL
                ) ENGINE=InnoDB"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Well-formed v4 UUIDs — the native MariaDB UUID column rejects
    // bad version/variant nibbles, so fixed values keep the test
    // deterministic.
    let uuids: Vec<Uuid> = (0..5u128)
        .map(|i| Uuid::from_u128(0x1000_0000_0000_4000_8000_0000_0000_0000_u128 + i))
        .collect();
    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let insert = format!(
        "INSERT INTO `{src_db}`.accounts \
         (id, ext, label, note, score, active, last_seen) \
         VALUES (?, ?, ?, ?, ?, ?, ?)"
    );
    for (i, u) in uuids.iter().enumerate() {
        let row = (i + 1) as i64;
        // Rotate NULLs across the nullable columns so each one hits
        // both NULL and present within the sample.
        let note: Option<String> = (i != 1).then(|| format!("note-{row}"));
        let score: Option<i32> = (i != 2).then_some(row as i32 * 7);
        let last_seen: Option<DateTime<Utc>> =
            (i != 3).then_some(base + chrono::Duration::seconds(row));
        sqlx::query(&insert)
            .bind(row)
            .bind(u.to_string())
            .bind(format!("label-{row}"))
            .bind(note)
            .bind(score)
            .bind(row % 2 == 0)
            .bind(last_seen)
            .execute(&mysql.pool)
            .await
            .unwrap();
    }

    let src_url = mysql.url_with_database();
    let dst_url = mariadb.url_with_database();

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "mysql"
config = {{ url = "{src_url}" }}

[[sinks]]
name = "snk"
type = "mysql"
config = {{ url = "{dst_url}" }}

[[storages]]
name = "st"
type = "mysql"
config = {{ url = "{dst_url}" }}

[flow.accounts]
source = "src"
sink = "snk"
storage = "st"
from = "{src_db}.accounts"
to = "{dst_db}.accounts"
batch-limit = 2

mapping = [
    {{ from = "id", to = "id" }},
    {{ from = "ext", to = "ext" }},
    {{ from = "label", to = "label" }},
    # Nullable source → NOT NULL sink, bridged by `default`.
    {{ from = "note", to = "note_safe", default = "n/a" }},
    {{ from = "score", to = "score" }},
    {{ from = "active", to = "active" }},
    {{ from = "last_seen", to = "last_seen" }},
]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // MariaDB returns native UUID as the canonical 36-char text on the
    // wire; decode and re-parse to compare.
    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        Vec<u8>,
        String,
        String,
        Option<i32>,
        bool,
        Option<DateTime<Utc>>,
    )> = sqlx::query_as(&format!(
        "SELECT id, ext, label, note_safe, score, active, last_seen \
         FROM `{dst_db}`.accounts ORDER BY id"
    ))
    .fetch_all(&mariadb.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.0, (i + 1) as i64);
        let text = std::str::from_utf8(&r.1).expect("uuid text");
        let got = Uuid::parse_str(text).expect("parse");
        assert_eq!(got, uuids[i], "Text→Uuid runtime conversion");
        assert_eq!(r.2, format!("label-{}", i + 1));
    }

    // Default substitution fired on row 2 (where `note` was NULL).
    assert_eq!(rows[1].3, "n/a", "default kicks in when source is NULL");
    assert_eq!(rows[0].3, "note-1", "non-NULL source preserved");

    // Genuinely nullable columns round-trip NULLs.
    assert!(rows[2].4.is_none(), "row 3 → score NULL");
    assert!(rows[3].6.is_none(), "row 4 → last_seen NULL");

    // Cursor lands on MariaDB via the legacy `VALUES()` UPSERT dialect.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&mariadb.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(5));

    mysql.pool.close().await;
    mariadb.pool.close().await;
}
