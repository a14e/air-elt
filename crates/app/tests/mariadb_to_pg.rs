//! Cross-vendor: MariaDB source → PostgreSQL sink, PostgreSQL storage.
//!
//! Closes the Hamilton cycle started by `pg_to_mongo`. Proves:
//!   * MariaDB source decodes a native `UUID` column (text on the wire,
//!     `Vec<u8>`-via-sqlx) into the canonical `Value::Uuid` and writes
//!     it into a PG `UUID` column without going through text,
//!   * mixed nullable + NOT NULL columns survive the round-trip across
//!     two physically distinct relational servers, with NULLs preserved
//!     on the nullable ones,
//!   * a NOT NULL sink column paired with a nullable source column is
//!     bridged via `default = "..."` on the mapping,
//!   * `VARBINARY → BYTEA` round-trip, including the empty-bytes edge
//!     case (sqlx's MySQL Bytes binding has historically dropped
//!     zero-length payloads — exercise the path here),
//!   * the source connector is the same `mysql` factory pointed at a
//!     MariaDB server, so the registry doesn't need a separate
//!     `type = "mariadb"`.

#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mariadb::mariadb_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::Executor;
use uuid::Uuid;

mod common;
use common::guard::{MysqlDbGuard, PgSchemaGuard};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mariadb_to_pg_with_uuid_and_mixed_nullability() {
    let mariadb = mariadb_pool().await;
    let pg = pg_pool().await;

    let src_db = format!("{}_src", mariadb.schema);
    let dst_schema = format!("{}_dst", pg.schema);

    let _src_guard = MysqlDbGuard::new(mariadb.pool.clone(), vec![src_db.clone()]);
    let _dst_guard = PgSchemaGuard::new(pg.pool.clone(), vec![dst_schema.clone()]);

    mariadb
        .pool
        .execute(format!("CREATE DATABASE `{src_db}`").as_str())
        .await
        .unwrap();
    mariadb
        .pool
        .execute(
            format!(
                "CREATE TABLE `{src_db}`.accounts (
                    id BIGINT NOT NULL PRIMARY KEY,
                    ext UUID NOT NULL,
                    label VARCHAR(64) NOT NULL,
                    payload_bytes VARBINARY(32) NOT NULL,
                    nickname VARCHAR(64),
                    visits BIGINT,
                    last_seen TIMESTAMP NULL
                ) ENGINE=InnoDB"
            )
            .as_str(),
        )
        .await
        .unwrap();

    pg.pool
        .execute(format!("CREATE SCHEMA \"{dst_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{dst_schema}\".accounts (
                    id BIGINT NOT NULL PRIMARY KEY,
                    ext UUID NOT NULL,
                    label TEXT NOT NULL,
                    payload_bytes BYTEA NOT NULL,
                    nickname_safe TEXT NOT NULL,
                    visits BIGINT,
                    last_seen TIMESTAMPTZ
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let uuids: Vec<Uuid> = (0..5u128)
        .map(|i| Uuid::from_u128(0x2000_0000_0000_4000_8000_0000_0000_0000_u128 + i))
        .collect();
    let base = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    for (i, u) in uuids.iter().enumerate() {
        let row = (i + 1) as i64;
        let nickname: Option<String> = (i != 1).then(|| format!("nick-{row}"));
        let visits: Option<i64> = (i != 2).then_some(row * 100);
        let last_seen: Option<DateTime<Utc>> =
            (i != 3).then_some(base + chrono::Duration::seconds(row));
        // Row 4: empty bytes — guards against the historical sqlx
        // zero-length-payload regression. Other rows: distinct fixed
        // patterns so a swap or truncation would surface as a diff.
        let payload_bytes: Vec<u8> = if i == 3 {
            Vec::new()
        } else {
            vec![row as u8; (row as usize) * 4]
        };
        sqlx::query(&format!(
            "INSERT INTO `{src_db}`.accounts \
             (id, ext, label, payload_bytes, nickname, visits, last_seen) \
             VALUES (?, ?, ?, ?, ?, ?, ?)"
        ))
        .bind(row)
        .bind(u.to_string())
        .bind(format!("label-{row}"))
        .bind(payload_bytes)
        .bind(nickname)
        .bind(visits)
        .bind(last_seen)
        .execute(&mariadb.pool)
        .await
        .unwrap();
    }

    let src_url = mariadb.url_with_database();
    let pg_url = pg.url_with_search_path();

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "mysql"
config = {{ url = "{src_url}" }}

[[sinks]]
name = "snk"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[storages]]
name = "st"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.accounts]
source = "src"
sink = "snk"
storage = "st"
from = "{src_db}.accounts"
to = "{dst_schema}.accounts"
batch-limit = 2

mapping = [
    {{ from = "id", to = "id" }},
    {{ from = "ext", to = "ext" }},
    {{ from = "label", to = "label" }},
    {{ from = "payload_bytes", to = "payload_bytes" }},
    # Nullable source → NOT NULL sink, bridged by `default`.
    {{ from = "nickname", to = "nickname_safe", default = "anonymous" }},
    {{ from = "visits", to = "visits" }},
    {{ from = "last_seen", to = "last_seen" }},
]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    common::pipeline::run_once(&config_path).await;

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        Uuid,
        String,
        Vec<u8>,
        String,
        Option<i64>,
        Option<DateTime<Utc>>,
    )> = sqlx::query_as(&format!(
        "SELECT id, ext, label, payload_bytes, nickname_safe, visits, last_seen \
         FROM \"{dst_schema}\".accounts ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);
    for (i, r) in rows.iter().enumerate() {
        assert_eq!(r.0, (i + 1) as i64);
        assert_eq!(r.1, uuids[i], "native UUID round-trips identity");
        assert_eq!(r.2, format!("label-{}", i + 1));
    }

    // VARBINARY → BYTEA round-trip, including the empty-bytes edge.
    assert_eq!(rows[0].3, vec![1_u8; 4]);
    assert_eq!(rows[2].3, vec![3_u8; 12]);
    assert!(rows[3].3.is_empty(), "row 4 → empty bytes survive");

    // Default substitution fires on row 2 (where `nickname` was NULL).
    assert_eq!(
        rows[1].4, "anonymous",
        "default kicks in when source is NULL"
    );
    assert_eq!(rows[0].4, "nick-1", "non-NULL source preserved");

    // Genuinely nullable columns round-trip NULLs.
    assert!(rows[2].5.is_none(), "row 3 → visits NULL");
    assert!(rows[3].6.is_none(), "row 4 → last_seen NULL");

    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(5));
}
