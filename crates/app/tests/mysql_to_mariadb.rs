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
use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sqlx::Executor;
use std::str::FromStr;
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
    // Source mixes nullable and NOT NULL columns and exercises the
    // full canonical type set the project supports on MySQL: signed
    // and unsigned integer widths, Float/Double, fixed and variable
    // text, fixed and variable bytes, sized and unbounded Bytes/Text
    // (via Blob / MediumText), Date, Timestamp, native UUID column
    // analogue (`VARCHAR(36)` reinterpreted on the sink), Json,
    // BigInt (`NUMERIC(20, 0)`) and Decimal (`NUMERIC(12, 4)`).
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
                    last_seen TIMESTAMP NULL,
                    tiny_signed TINYINT NOT NULL,
                    small_signed SMALLINT NOT NULL,
                    medium_signed INT NOT NULL,
                    tiny_unsigned TINYINT UNSIGNED NOT NULL,
                    small_unsigned SMALLINT UNSIGNED NOT NULL,
                    int_unsigned INT UNSIGNED NOT NULL,
                    big_unsigned BIGINT UNSIGNED NOT NULL,
                    real_f FLOAT NOT NULL,
                    double_f DOUBLE NOT NULL,
                    big_decimal NUMERIC(20, 0) NOT NULL,
                    fixed_decimal NUMERIC(12, 4) NOT NULL,
                    fixed_blob VARBINARY(8) NOT NULL,
                    blob_unbounded BLOB NOT NULL,
                    medium_text MEDIUMTEXT NOT NULL,
                    born_on DATE NOT NULL,
                    payload JSON NOT NULL,
                    big_count BIGINT NOT NULL,
                    long_bio MEDIUMTEXT NOT NULL
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
    // Sink mirrors the source widths for round-trip columns, narrows
    // a couple of pairs through `truncate=true` (Int64 → Int32 and
    // unbounded-Text → bounded-VARCHAR), and introduces a NOT NULL
    // `note_safe` backed by `default` to verify nullable_src →
    // not_null_sink works.
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
                    last_seen TIMESTAMP NULL,
                    tiny_signed TINYINT NOT NULL,
                    small_signed SMALLINT NOT NULL,
                    medium_signed INT NOT NULL,
                    tiny_unsigned TINYINT UNSIGNED NOT NULL,
                    small_unsigned SMALLINT UNSIGNED NOT NULL,
                    int_unsigned INT UNSIGNED NOT NULL,
                    big_unsigned BIGINT UNSIGNED NOT NULL,
                    real_f FLOAT NOT NULL,
                    double_f DOUBLE NOT NULL,
                    big_decimal NUMERIC(20, 0) NOT NULL,
                    fixed_decimal NUMERIC(12, 4) NOT NULL,
                    fixed_blob VARBINARY(8) NOT NULL,
                    blob_unbounded BLOB NOT NULL,
                    medium_text MEDIUMTEXT NOT NULL,
                    born_on DATE NOT NULL,
                    payload JSON NOT NULL,
                    -- truncate: Int64 source value fits Int32 sink.
                    big_count_narrow INT NOT NULL,
                    -- truncate: MEDIUMTEXT source narrowed to VARCHAR(8).
                    long_bio_clipped VARCHAR(8) NOT NULL
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
         (id, ext, label, note, score, active, last_seen, \
          tiny_signed, small_signed, medium_signed, \
          tiny_unsigned, small_unsigned, int_unsigned, big_unsigned, \
          real_f, double_f, big_decimal, fixed_decimal, \
          fixed_blob, blob_unbounded, medium_text, born_on, payload, \
          big_count, long_bio) \
         VALUES (?, ?, ?, ?, ?, ?, ?, \
                 ?, ?, ?, \
                 ?, ?, ?, ?, \
                 ?, ?, ?, ?, \
                 ?, ?, ?, ?, ?, \
                 ?, ?)"
    );
    // Per-row "long bio" — pad to >8 chars so the `VARCHAR(8)` sink
    // truncation path actually fires on every row.
    let long_bio = "biography-text-that-definitely-exceeds-eight-bytes";
    for (i, u) in uuids.iter().enumerate() {
        let row = (i + 1) as i64;
        // Rotate NULLs across the nullable columns so each one hits
        // both NULL and present within the sample.
        let note: Option<String> = (i != 1).then(|| format!("note-{row}"));
        let score: Option<i32> = (i != 2).then_some(row as i32 * 7);
        let last_seen: Option<DateTime<Utc>> =
            (i != 3).then_some(base + chrono::Duration::seconds(row));
        let payload = serde_json::json!({ "row": row, "label": format!("label-{row}") });
        let born_on = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(row);
        let big_decimal = BigDecimal::from(10_000_000_000_000_000_i64 + row);
        let fixed_decimal = BigDecimal::from_str(&format!("{}.{:04}", row * 10, row)).unwrap();
        sqlx::query(&insert)
            .bind(row)
            .bind(u.to_string())
            .bind(format!("label-{row}"))
            .bind(note)
            .bind(score)
            .bind(row % 2 == 0)
            .bind(last_seen)
            // tiny / small / medium signed
            .bind(row as i8)
            .bind(row as i16 * 100)
            .bind(row as i32 * 1_000)
            // unsigned: pick row + offsets to stay well within bounds.
            .bind(row as u8 + 10)
            .bind(row as u16 * 50)
            .bind(row as u32 * 1_000_000)
            .bind(row as u64 * 2_000_000_000)
            // floats
            .bind(row as f32 * 1.5_f32)
            .bind(row as f64 * 1.25_f64)
            // numeric / decimal
            .bind(&big_decimal)
            .bind(&fixed_decimal)
            // bytes: VARBINARY(8) — exactly fits, blob unbounded — arbitrary
            .bind(vec![row as u8; 4])
            .bind(format!("blob-{row}").into_bytes())
            // medium text round-trip
            .bind(format!("medium-{row}-padding"))
            .bind(born_on)
            .bind(&payload)
            // Int64 source value that fits Int32: well below 2^31.
            .bind(row * 1_000)
            // unbounded text source for VARCHAR(8) truncate sink.
            .bind(long_bio)
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

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.accounts.mapping]
id = "id"
ext = "ext"
label = "label"
# Nullable source → NOT NULL sink, bridged by `default`.
note_safe = {{ from = "note", default = "n/a" }}
score = "score"
active = "active"
last_seen = "last_seen"
tiny_signed = "tiny_signed"
small_signed = "small_signed"
medium_signed = "medium_signed"
tiny_unsigned = "tiny_unsigned"
small_unsigned = "small_unsigned"
int_unsigned = "int_unsigned"
big_unsigned = "big_unsigned"
real_f = "real_f"
double_f = "double_f"
big_decimal = "big_decimal"
fixed_decimal = "fixed_decimal"
fixed_blob = "fixed_blob"
blob_unbounded = "blob_unbounded"
medium_text = "medium_text"
born_on = "born_on"
# MariaDB's JSON column type is LONGTEXT under the hood, so the sink
# schema introspection reports a bounded Text; Json -> Text(n) is gated
# by truncate=true.
payload = {{ from = "payload", truncate = true }}
# truncate=true: Int64 → Int32 (value fits, but matrix demands consent).
big_count_narrow = {{ from = "big_count", truncate = true }}
# truncate=true: MEDIUMTEXT source narrowed to VARCHAR(8).
long_bio_clipped = {{ from = "long_bio", truncate = true }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // MariaDB returns native UUID as the canonical 36-char text on the
    // wire; decode and re-parse to compare. Use `sqlx::Row::try_get`
    // per column rather than a tuple `query_as` because the result row
    // has more than 16 columns (sqlx's tuple `FromRow` impls top out
    // at arity 16).
    use sqlx::Row as _;
    let rows = sqlx::query(&format!(
        "SELECT id, ext, label, note_safe, score, active, last_seen, \
                tiny_signed, small_signed, medium_signed, \
                tiny_unsigned, small_unsigned, int_unsigned, big_unsigned, \
                real_f, double_f, big_decimal, fixed_decimal, \
                fixed_blob, blob_unbounded, medium_text, born_on, payload, \
                big_count_narrow, long_bio_clipped \
         FROM `{dst_db}`.accounts ORDER BY id"
    ))
    .fetch_all(&mariadb.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);
    for (i, r) in rows.iter().enumerate() {
        let row = (i + 1) as i64;
        let id: i64 = r.try_get("id").unwrap();
        let ext: Vec<u8> = r.try_get("ext").unwrap();
        let label: String = r.try_get("label").unwrap();
        let note_safe: String = r.try_get("note_safe").unwrap();
        let score: Option<i32> = r.try_get("score").unwrap();
        let active: bool = r.try_get("active").unwrap();
        let last_seen: Option<DateTime<Utc>> = r.try_get("last_seen").unwrap();
        let tiny_signed: i8 = r.try_get("tiny_signed").unwrap();
        let small_signed: i16 = r.try_get("small_signed").unwrap();
        let medium_signed: i32 = r.try_get("medium_signed").unwrap();
        let tiny_unsigned: u8 = r.try_get("tiny_unsigned").unwrap();
        let small_unsigned: u16 = r.try_get("small_unsigned").unwrap();
        let int_unsigned: u32 = r.try_get("int_unsigned").unwrap();
        let big_unsigned: u64 = r.try_get("big_unsigned").unwrap();
        let real_f: f32 = r.try_get("real_f").unwrap();
        let double_f: f64 = r.try_get("double_f").unwrap();
        let big_decimal: BigDecimal = r.try_get("big_decimal").unwrap();
        let fixed_decimal: BigDecimal = r.try_get("fixed_decimal").unwrap();
        let fixed_blob: Vec<u8> = r.try_get("fixed_blob").unwrap();
        let blob_unbounded: Vec<u8> = r.try_get("blob_unbounded").unwrap();
        let medium_text: String = r.try_get("medium_text").unwrap();
        let born_on: NaiveDate = r.try_get("born_on").unwrap();
        // MariaDB's `JSON` storage is LONGTEXT; the runner truncated the
        // canonical JSON encoding into the bounded sink column, which
        // sqlx surfaces as a BLOB on read. Decode the bytes as UTF-8
        // and parse the canonical JSON form.
        let payload_bytes: Vec<u8> = r.try_get("payload").unwrap();
        let payload: serde_json::Value =
            serde_json::from_slice(&payload_bytes).expect("payload re-parse");
        let big_count_narrow: i32 = r.try_get("big_count_narrow").unwrap();
        let long_bio_clipped: String = r.try_get("long_bio_clipped").unwrap();

        assert_eq!(id, row);
        let text = std::str::from_utf8(&ext).expect("uuid text");
        let got = Uuid::parse_str(text).expect("parse");
        assert_eq!(got, uuids[i], "Text→Uuid runtime conversion");
        assert_eq!(label, format!("label-{}", i + 1));

        // Nullable columns: per-row rotating NULL placement.
        if i == 1 {
            assert_eq!(note_safe, "n/a", "default kicks in when source is NULL");
        } else {
            assert_eq!(
                note_safe,
                format!("note-{row}"),
                "non-NULL source preserved"
            );
        }
        if i == 2 {
            assert!(score.is_none(), "row 3 → score NULL");
        } else {
            assert_eq!(score, Some(row as i32 * 7));
        }
        assert_eq!(active, row % 2 == 0);
        if i == 3 {
            assert!(last_seen.is_none(), "row 4 → last_seen NULL");
        } else {
            assert_eq!(
                last_seen,
                Some(base + chrono::Duration::seconds(row)),
                "last_seen round-trips"
            );
        }

        // Signed integers.
        assert_eq!(tiny_signed, row as i8, "tiny_signed round-trips");
        assert_eq!(small_signed, row as i16 * 100, "small_signed round-trips");
        assert_eq!(
            medium_signed,
            row as i32 * 1_000,
            "medium_signed round-trips"
        );
        // Unsigned integers.
        assert_eq!(tiny_unsigned, row as u8 + 10, "tiny_unsigned round-trips");
        assert_eq!(
            small_unsigned,
            row as u16 * 50,
            "small_unsigned round-trips"
        );
        assert_eq!(
            int_unsigned,
            row as u32 * 1_000_000,
            "int_unsigned round-trips"
        );
        assert_eq!(
            big_unsigned,
            row as u64 * 2_000_000_000,
            "big_unsigned round-trips"
        );
        // Floats — use exact equality because every input is an exact
        // multiple representable as f32/f64.
        assert_eq!(real_f, row as f32 * 1.5_f32, "real_f round-trips");
        assert_eq!(double_f, row as f64 * 1.25_f64, "double_f round-trips");
        // Numeric / Decimal.
        assert_eq!(
            big_decimal,
            BigDecimal::from(10_000_000_000_000_000_i64 + row),
            "big_decimal (NUMERIC(20,0) → BigInt) round-trips"
        );
        assert_eq!(
            fixed_decimal,
            BigDecimal::from_str(&format!("{}.{:04}", row * 10, row)).unwrap(),
            "fixed_decimal (NUMERIC(12,4)) round-trips"
        );
        // Bytes.
        assert_eq!(fixed_blob, vec![row as u8; 4], "fixed_blob round-trips");
        assert_eq!(
            blob_unbounded,
            format!("blob-{row}").into_bytes(),
            "blob_unbounded round-trips"
        );
        // MEDIUMTEXT identity.
        assert_eq!(medium_text, format!("medium-{row}-padding"));
        // Date.
        assert_eq!(
            born_on,
            NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(row),
            "born_on round-trips"
        );
        // JSON — Mysql normalises whitespace; compare as parsed values.
        let expected_payload = serde_json::json!({ "row": row, "label": format!("label-{row}") });
        assert_eq!(payload, expected_payload, "JSON round-trips");

        // Truncate path 1: Int64 → Int32 with an in-range source value.
        assert_eq!(
            big_count_narrow,
            (row * 1_000) as i32,
            "Int64 → Int32 truncate (value fits) preserves value"
        );
        // Truncate path 2: unbounded-text → VARCHAR(8) clips to 8 chars.
        assert_eq!(
            long_bio_clipped.chars().count(),
            8,
            "Text(unbounded) → Text(8) truncates to declared width"
        );
        assert!(
            long_bio.starts_with(long_bio_clipped.as_str()),
            "truncated prefix must match the source's leading 8 chars"
        );
    }

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

/// AIR-70 `switch` expression with integer keys, struct-to-struct
/// (mysql → mariadb). The third row's `code` misses the switch and
/// must materialise as the `default` arm value.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mysql_to_mariadb_switch_with_default_arm() {
    let mysql = mysql_pool().await;
    let mariadb = mariadb_pool().await;

    let src_db = format!("{}_sw_src", mysql.schema);
    let dst_db = format!("{}_sw_dst", mariadb.schema);

    mysql
        .pool
        .execute(format!("CREATE DATABASE `{src_db}`").as_str())
        .await
        .unwrap();
    mysql
        .pool
        .execute(
            format!(
                "CREATE TABLE `{src_db}`.events (
                    id   BIGINT NOT NULL,
                    code INT NOT NULL,
                    note TEXT
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
    mariadb
        .pool
        .execute(
            format!(
                "CREATE TABLE `{dst_db}`.events_labelled (
                    id          BIGINT,
                    label       TEXT,
                    env_default TEXT,
                    interp_switch TEXT,
                    multi_if    TEXT,
                    null_check  TEXT
                ) ENGINE=InnoDB"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let insert = format!("INSERT INTO `{src_db}`.events (id, code) VALUES (?, ?)");
    let fixtures: [(i64, i32); 3] = [(1, 10), (2, 20), (3, 99)];
    for (id, code) in fixtures {
        sqlx::query(&insert)
            .bind(id)
            .bind(code)
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

[flow.events]
source = "src"
sink = "snk"
storage = "st"
from = "{src_db}.events"
to = "{dst_db}.events_labelled"
batch-limit = 8

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}

[flow.events.mapping]
id = "id"
label = {{ from = "code", switch = {{ 10 = "alpha", 20 = "beta" }}, default = "other" }}
env_default = {{ from = "note", default = "env('AIR_ELT_TEST_LABEL', 'fallback')" }}
interp_switch = {{ from = "code", switch = {{ 10 = "alpha-{{toString(10)}}", 20 = "beta-{{toString(20)}}" }}, default = "code-{{toString(0)}}" }}
multi_if = {{ from = "note", default = "multiIf(1 > 2, 'big', 1 == 1, 'equal', 'other')" }}
null_check = {{ from = "note", default = "ifNull(null, 'was-null')" }}
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    #[allow(clippy::type_complexity)]
    let rows: Vec<(
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    )> = sqlx::query_as(&format!(
        "SELECT id, label, env_default, interp_switch, multi_if, null_check \
         FROM `{dst_db}`.events_labelled ORDER BY id"
    ))
    .fetch_all(&mariadb.pool)
    .await
    .unwrap();

    assert_eq!(rows.len(), 3);

    // Switch: int keys with default
    assert_eq!(rows[0].1, Some("alpha".to_string()));
    assert_eq!(rows[1].1, Some("beta".to_string()));
    assert_eq!(
        rows[2].1,
        Some("other".to_string()),
        "miss must fall back to `default`"
    );

    // env() default (note is NULL → default fires, env var not set → fallback)
    for row in &rows {
        assert_eq!(row.2, Some("fallback".to_string()));
    }

    // Interpolation in switch values
    assert_eq!(rows[0].3, Some("alpha-10".to_string()));
    assert_eq!(rows[1].3, Some("beta-20".to_string()));
    assert_eq!(rows[2].3, Some("code-0".to_string()));

    // multiIf: 1 > 2 false, 1 == 1 true → "equal"
    for row in &rows {
        assert_eq!(row.4, Some("equal".to_string()));
    }

    // ifNull(null, 'was-null') → "was-null"
    for row in &rows {
        assert_eq!(row.5, Some("was-null".to_string()));
    }

    mysql.pool.close().await;
    mariadb.pool.close().await;
}
