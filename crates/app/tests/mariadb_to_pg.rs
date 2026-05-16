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

use air_elt_app::App;
use air_elt_commons_testing::mariadb::mariadb_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use sqlx::Executor;
use std::str::FromStr;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mariadb_to_pg_with_uuid_and_mixed_nullability() {
    let mariadb = mariadb_pool().await;
    let pg = pg_pool().await;

    let src_db = format!("{}_src", mariadb.schema);
    let dst_schema = format!("{}_dst", pg.schema);

    mariadb
        .pool
        .execute(format!("CREATE DATABASE `{src_db}`").as_str())
        .await
        .unwrap();
    // Source exercises the full canonical type set the MariaDB
    // (`mysql` connector) source produces: signed and unsigned integer
    // widths, Float/Double, fixed and unbounded text (VARCHAR /
    // MEDIUMTEXT), fixed and unbounded bytes (VARBINARY / BLOB), Date,
    // Timestamp, native UUID (MariaDB 10.7+), Bool (`tinyint(1)`),
    // BigInt (`NUMERIC(20, 0)`) and Decimal (`NUMERIC(12, 4)`).
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
                    last_seen TIMESTAMP NULL,
                    is_active TINYINT(1) NOT NULL,
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
                    blob_unbounded BLOB NOT NULL,
                    medium_text MEDIUMTEXT NOT NULL,
                    born_on DATE NOT NULL,
                    big_count BIGINT NOT NULL,
                    long_bio MEDIUMTEXT NOT NULL
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
    // Sink mirrors source widths for round-trip columns, widens MySQL
    // unsigned types into the next signed/BigInt step (PG has no
    // unsigned int types), and exercises two `truncate=true` columns:
    // BIGINT -> INTEGER (in-range source values) and MEDIUMTEXT
    // (unbounded-ish) -> VARCHAR(8).
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
                    last_seen TIMESTAMPTZ,
                    is_active BOOLEAN NOT NULL,
                    tiny_signed SMALLINT NOT NULL,
                    small_signed SMALLINT NOT NULL,
                    medium_signed INTEGER NOT NULL,
                    -- Unsigned widens one step: UInt8 -> Int16, etc.
                    tiny_unsigned SMALLINT NOT NULL,
                    small_unsigned INTEGER NOT NULL,
                    int_unsigned BIGINT NOT NULL,
                    big_unsigned NUMERIC(20, 0) NOT NULL,
                    real_f REAL NOT NULL,
                    double_f DOUBLE PRECISION NOT NULL,
                    big_decimal NUMERIC(20, 0) NOT NULL,
                    fixed_decimal NUMERIC(12, 4) NOT NULL,
                    blob_unbounded BYTEA NOT NULL,
                    medium_text TEXT NOT NULL,
                    born_on DATE NOT NULL,
                    -- truncate: BIGINT -> INTEGER (in-range values).
                    big_count_narrow INTEGER NOT NULL,
                    -- truncate: MEDIUMTEXT -> VARCHAR(8).
                    long_bio_clipped VARCHAR(8) NOT NULL
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
    let long_bio = "biography-text-that-definitely-exceeds-eight-bytes";
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
        let born_on = NaiveDate::from_ymd_opt(2020, 1, 1).unwrap() + chrono::Duration::days(row);
        let big_decimal = BigDecimal::from(10_000_000_000_000_000_i64 + row);
        let fixed_decimal = BigDecimal::from_str(&format!("{}.{:04}", row * 10, row)).unwrap();
        sqlx::query(&format!(
            "INSERT INTO `{src_db}`.accounts \
             (id, ext, label, payload_bytes, nickname, visits, last_seen, \
              is_active, tiny_signed, small_signed, medium_signed, \
              tiny_unsigned, small_unsigned, int_unsigned, big_unsigned, \
              real_f, double_f, big_decimal, fixed_decimal, \
              blob_unbounded, medium_text, born_on, big_count, long_bio) \
             VALUES (?, ?, ?, ?, ?, ?, ?, \
                     ?, ?, ?, ?, \
                     ?, ?, ?, ?, \
                     ?, ?, ?, ?, \
                     ?, ?, ?, ?, ?)"
        ))
        .bind(row)
        .bind(u.to_string())
        .bind(format!("label-{row}"))
        .bind(payload_bytes)
        .bind(nickname)
        .bind(visits)
        .bind(last_seen)
        // boolean (tinyint(1))
        .bind(row % 2 == 0)
        // signed
        .bind(row as i8)
        .bind(row as i16 * 100)
        .bind(row as i32 * 1_000)
        // unsigned (stays in-range)
        .bind(row as u8 + 10)
        .bind(row as u16 * 50)
        .bind(row as u32 * 1_000_000)
        .bind(row as u64 * 2_000_000_000)
        // floats — exact multiples
        .bind(row as f32 * 1.5_f32)
        .bind(row as f64 * 1.25_f64)
        // numeric / decimal
        .bind(&big_decimal)
        .bind(&fixed_decimal)
        // unbounded bytes + medium text
        .bind(format!("blob-{row}").into_bytes())
        .bind(format!("medium-{row}-padding"))
        .bind(born_on)
        // truncate sources: Int64 value fitting in Int32, long text
        .bind(row * 1_000)
        .bind(long_bio)
        .execute(&mariadb.pool)
        .await
        .unwrap();
    }

    let src_url = mariadb.url_with_database();
    let pg_url = pg.url_with_search_path();

    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: mysql
    config:
      url: "{src_url}"

sinks:
  - name: snk
    type: postgres
    config:
      url: "{pg_url}"

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"

flow:
  accounts:
    source: src
    sink: snk
    storage: st
    from: "{src_db}.accounts"
    to: "{dst_schema}.accounts"
    batch-limit: 2

    mapping:
      id: id
      ext: ext
      label: label
      payload_bytes: payload_bytes
      # Nullable source -> NOT NULL sink, bridged by `default`.
      nickname_safe: {{ from: nickname, default: anonymous }}
      visits: visits
      last_seen: last_seen
      is_active: is_active
      tiny_signed: tiny_signed
      small_signed: small_signed
      medium_signed: medium_signed
      tiny_unsigned: tiny_unsigned
      small_unsigned: small_unsigned
      int_unsigned: int_unsigned
      big_unsigned: big_unsigned
      real_f: real_f
      double_f: double_f
      big_decimal: big_decimal
      fixed_decimal: fixed_decimal
      blob_unbounded: blob_unbounded
      medium_text: medium_text
      born_on: born_on
      # truncate=true: Int64 -> Int32 with values that fit i32.
      big_count_narrow: {{ from: big_count, truncate: true }}
      # truncate=true: MEDIUMTEXT -> VARCHAR(8) clips to 8 chars.
      long_bio_clipped: {{ from: long_bio, truncate: true }}

    cursor:
      fields: [id]
      order: asc
      interval: "100ms"
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // The result row has more than 16 columns — sqlx tuple `FromRow`
    // tops out at arity 16, so fetch via `sqlx::Row::try_get`.
    use sqlx::Row as _;
    let rows = sqlx::query(&format!(
        "SELECT id, ext, label, payload_bytes, nickname_safe, visits, last_seen, \
                is_active, tiny_signed, small_signed, medium_signed, \
                tiny_unsigned, small_unsigned, int_unsigned, big_unsigned, \
                real_f, double_f, big_decimal, fixed_decimal, \
                blob_unbounded, medium_text, born_on, \
                big_count_narrow, long_bio_clipped \
         FROM \"{dst_schema}\".accounts ORDER BY id"
    ))
    .fetch_all(&pg.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);
    for (i, r) in rows.iter().enumerate() {
        let row = (i + 1) as i64;
        let id: i64 = r.try_get("id").unwrap();
        let ext: Uuid = r.try_get("ext").unwrap();
        let label: String = r.try_get("label").unwrap();
        // `payload_bytes` is checked separately after the loop (it has
        // a row-dependent shape, including the empty-bytes edge case).
        let nickname_safe: String = r.try_get("nickname_safe").unwrap();
        let visits: Option<i64> = r.try_get("visits").unwrap();
        let last_seen: Option<DateTime<Utc>> = r.try_get("last_seen").unwrap();
        let is_active: bool = r.try_get("is_active").unwrap();
        let tiny_signed: i16 = r.try_get("tiny_signed").unwrap();
        let small_signed: i16 = r.try_get("small_signed").unwrap();
        let medium_signed: i32 = r.try_get("medium_signed").unwrap();
        let tiny_unsigned: i16 = r.try_get("tiny_unsigned").unwrap();
        let small_unsigned: i32 = r.try_get("small_unsigned").unwrap();
        let int_unsigned: i64 = r.try_get("int_unsigned").unwrap();
        let big_unsigned: BigDecimal = r.try_get("big_unsigned").unwrap();
        let real_f: f32 = r.try_get("real_f").unwrap();
        let double_f: f64 = r.try_get("double_f").unwrap();
        let big_decimal: BigDecimal = r.try_get("big_decimal").unwrap();
        let fixed_decimal: BigDecimal = r.try_get("fixed_decimal").unwrap();
        let blob_unbounded: Vec<u8> = r.try_get("blob_unbounded").unwrap();
        let medium_text: String = r.try_get("medium_text").unwrap();
        let born_on: NaiveDate = r.try_get("born_on").unwrap();
        let big_count_narrow: i32 = r.try_get("big_count_narrow").unwrap();
        let long_bio_clipped: String = r.try_get("long_bio_clipped").unwrap();

        assert_eq!(id, row);
        assert_eq!(ext, uuids[i], "native UUID round-trips identity");
        assert_eq!(label, format!("label-{}", i + 1));

        // Nullable columns.
        if i == 1 {
            assert_eq!(
                nickname_safe, "anonymous",
                "default kicks in when source is NULL"
            );
        } else {
            assert_eq!(nickname_safe, format!("nick-{row}"));
        }
        if i == 2 {
            assert!(visits.is_none(), "row 3 → visits NULL");
        } else {
            assert_eq!(visits, Some(row * 100));
        }
        if i == 3 {
            assert!(last_seen.is_none(), "row 4 → last_seen NULL");
        } else {
            assert_eq!(last_seen, Some(base + chrono::Duration::seconds(row)));
        }

        // Bool from tinyint(1).
        assert_eq!(is_active, row % 2 == 0);
        // Signed widened (Int8 → Int16 sink) / identity.
        assert_eq!(tiny_signed, row as i16, "tiny_signed widened Int8 → Int16");
        assert_eq!(small_signed, row as i16 * 100);
        assert_eq!(medium_signed, row as i32 * 1_000);
        // Unsigned widened one step.
        assert_eq!(tiny_unsigned, (row as u8 + 10) as i16);
        assert_eq!(small_unsigned, (row as u16 * 50) as i32);
        assert_eq!(int_unsigned, (row as u32 * 1_000_000) as i64);
        assert_eq!(
            big_unsigned,
            BigDecimal::from(row as u64 * 2_000_000_000),
            "UInt64 → NUMERIC(20,0) round-trips"
        );
        // Floats.
        assert_eq!(real_f, row as f32 * 1.5_f32);
        assert_eq!(double_f, row as f64 * 1.25_f64);
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
        // Bytes unbounded round-trip.
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
        );
        // Truncate: Int64 → Int32 in-range.
        assert_eq!(
            big_count_narrow,
            (row * 1_000) as i32,
            "Int64 → Int32 truncate (value fits) preserves value"
        );
        // Truncate: MEDIUMTEXT → VARCHAR(8).
        assert_eq!(
            long_bio_clipped.chars().count(),
            8,
            "Text source narrowed to VARCHAR(8)"
        );
        assert!(long_bio.starts_with(long_bio_clipped.as_str()));
    }

    // VARBINARY → BYTEA round-trip, including the empty-bytes edge.
    let bytes_0: Vec<u8> = rows[0].try_get("payload_bytes").unwrap();
    let bytes_2: Vec<u8> = rows[2].try_get("payload_bytes").unwrap();
    let bytes_3: Vec<u8> = rows[3].try_get("payload_bytes").unwrap();
    assert_eq!(bytes_0, vec![1_u8; 4]);
    assert_eq!(bytes_2, vec![3_u8; 12]);
    assert!(bytes_3.is_empty(), "row 4 → empty bytes survive");

    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(5));

    mariadb.pool.close().await;
    pg.pool.close().await;
}
