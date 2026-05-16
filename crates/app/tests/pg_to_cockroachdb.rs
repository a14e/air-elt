//! Cross-engine: PostgreSQL source → CockroachDB sink, CockroachDB storage.
//!
//! Exercises the alias-on-postgres + `Dialect::Cockroach` path end-to-end
//! through the CLI/engine code path:
//!   * the registry resolves `type = "cockroachdb"` to the same `PgSink`/
//!     `PgStorage` types but with `Dialect::Cockroach`,
//!   * `Storage::migrate` finds and applies `migrations/storage-cockroachdb/`
//!     (with advisory-lock disabled — Cockroach has no `pg_advisory_lock`),
//!   * cursor save/load round-trips through CockroachDB's JSONB,
//!   * `INSERT … ON CONFLICT (id) DO UPDATE` runs against Cockroach with
//!     the same semantics it has on Postgres — overwrite of a pre-existing
//!     row succeeds.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::types::Value;
use chrono::{NaiveDate, TimeZone};
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_cockroachdb_with_upsert_overwrite() {
    let pg = pg_pool().await;
    let cockroach = cockroach_pool().await;

    let src_schema = format!("{}_src", pg.schema);

    // Source carries the full canonical type vocabulary PG produces — minus
    // XML, which Cockroach's dialect rejects upfront (Dialect::Cockroach
    // marks XML as unsupported in `validate_access`). The wide-type set
    // here proves the cockroach sink consumes every PG canonical type via
    // its standard `INSERT … ON CONFLICT … DO UPDATE` upsert path.
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".users (
                    id            BIGINT PRIMARY KEY,
                    email         TEXT NOT NULL,
                    display_name  TEXT NOT NULL,
                    is_active     BOOLEAN NOT NULL,
                    rating_small  SMALLINT NOT NULL,
                    score32       REAL NOT NULL,
                    score64       DOUBLE PRECISION NOT NULL,
                    huge_count    NUMERIC(20, 0) NOT NULL,
                    balance       NUMERIC(10, 2) NOT NULL,
                    handle        VARCHAR(8) NOT NULL,
                    avatar        BYTEA NOT NULL,
                    avatar_big    BYTEA,
                    born_on       DATE NOT NULL,
                    last_seen     TIMESTAMPTZ NOT NULL,
                    public_id     UUID NOT NULL,
                    meta          JSONB,
                    bio           TEXT,
                    legacy_big    BIGINT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    // Cockroach sandbox database is dropped by the handle's Drop.
    // `bio` sink column is intentionally narrower (`VARCHAR(5)`) and
    // `legacy_big` is `INT4` so the mapping can opt them into truncate.
    cockroach
        .pool
        .execute(
            "CREATE TABLE users (
                id            INT8 PRIMARY KEY,
                email         STRING NOT NULL,
                display_name  STRING NOT NULL,
                is_active     BOOL NOT NULL,
                rating_small  INT2 NOT NULL,
                score32       FLOAT4 NOT NULL,
                score64       FLOAT8 NOT NULL,
                huge_count    DECIMAL(20, 0) NOT NULL,
                balance       DECIMAL(10, 2) NOT NULL,
                handle        VARCHAR(8) NOT NULL,
                avatar        BYTES NOT NULL,
                avatar_big    BYTES,
                born_on       DATE NOT NULL,
                last_seen     TIMESTAMPTZ NOT NULL,
                public_id     UUID NOT NULL,
                meta          JSONB,
                bio           VARCHAR(5),
                legacy_big    INT4 NOT NULL
            )",
        )
        .await
        .unwrap();
    // Pre-existing row that the sink must overwrite via the upsert path.
    cockroach
        .pool
        .execute(
            "INSERT INTO users (
                id, email, display_name, is_active, rating_small, score32, score64,
                huge_count, balance, handle, avatar, avatar_big, born_on, last_seen,
                public_id, meta, bio, legacy_big
            ) VALUES (
                1, 'old@x', 'stale', false, 0, 0.0, 0.0,
                0, 0.00, 'old', b'\\x00', NULL, '1900-01-01', '1900-01-01T00:00:00Z',
                '00000000-0000-0000-0000-000000000000', NULL, NULL, 0
            )",
        )
        .await
        .unwrap();

    let base_ts = chrono::Utc.with_ymd_and_hms(2026, 5, 1, 10, 0, 0).unwrap();
    for i in 1..=5_i64 {
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".users (
                id, email, display_name, is_active, rating_small,
                score32, score64, huge_count, balance, handle,
                avatar, avatar_big, born_on, last_seen, public_id,
                meta, bio, legacy_big
             ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8::numeric, $9::numeric, $10,
                $11, $12, $13, $14, $15,
                $16, $17, $18
             )"
        ))
        .bind(i)
        .bind(format!("user{i}@example.com"))
        .bind(format!("User {i}"))
        .bind(i % 2 == 1)
        .bind(i as i16)
        .bind(i as f32 + 0.5)
        .bind(i as f64 * 1.25)
        .bind(
            format!("{i}9999999999999999999")
                .parse::<bigdecimal::BigDecimal>()
                .unwrap(),
        )
        .bind(format!("{i}.50").parse::<bigdecimal::BigDecimal>().unwrap())
        .bind(format!("h{i}"))
        .bind(vec![i as u8, (i + 1) as u8])
        .bind(if i == 3 {
            None
        } else {
            Some(vec![0xab_u8; i as usize])
        })
        .bind(NaiveDate::from_ymd_opt(1990, 1, i as u32).unwrap())
        .bind(base_ts + chrono::Duration::seconds(i))
        .bind(uuid::Uuid::from_u128(
            0xa0eebc99_9c0b_4ef8_bb6d_6bb9bd380a00 + i as u128,
        ))
        .bind(if i == 4 {
            None
        } else {
            Some(serde_json::json!({ "row": i }))
        })
        .bind(if i == 5 {
            None
        } else {
            Some(format!("long-bio-{i}"))
        })
        .bind(1_000_i64 * i)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    let pg_url = pg.url_with_search_path();
    let cockroach_url = cockroach.url_with_database();

    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: cockroachdb
    config:
      url: "{cockroach_url}"

storages:
  - name: st
    type: cockroachdb
    config:
      url: "{cockroach_url}"

flow:
  users:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.users"
    to: "public.users"
    batch-limit: 2

    mapping:
      id: id
      email: email
      display_name: display_name
      is_active: is_active
      rating_small: rating_small
      score32: score32
      score64: score64
      huge_count: huge_count
      balance: balance
      handle: handle
      avatar: avatar
      avatar_big: avatar_big
      born_on: born_on
      last_seen: last_seen
      public_id: public_id
      meta: meta
      # Source `bio` is nullable; default fires when the source value is NULL.
      bio: {{ from: bio, truncate: true, default: "n/a" }}
      # Source `legacy_big` is NOT NULL; truncate only — no default.
      legacy_big: {{ from: legacy_big, truncate: true }}

    cursor:
      fields: [id]
      order: asc
      interval: "100ms"

    # Single-key Overwrite -- drives the sink down the upsert path on Cockroach.
    conflict:
      key: [id]
      strategy: overwrite
"#,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // 5 rows landed; row id=1 was overwritten (stale → fresh email).
    use sqlx::Row as _;
    let rows = sqlx::query(
        "SELECT id, email, display_name, is_active, rating_small, \
                score32, score64, huge_count::text, balance::text, handle, \
                avatar, avatar_big, born_on, last_seen, public_id, meta, \
                bio, legacy_big \
         FROM users ORDER BY id",
    )
    .fetch_all(&cockroach.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 5);

    let r1 = &rows[0];
    assert_eq!(r1.get::<i64, _>(0), 1);
    assert_eq!(r1.get::<String, _>(1), "user1@example.com");
    assert_eq!(r1.get::<String, _>(2), "User 1");
    assert!(r1.get::<bool, _>(3), "is_active row 1");
    assert_eq!(r1.get::<i16, _>(4), 1);
    let row1_huge: String = r1.get(7);
    assert_eq!(row1_huge, "19999999999999999999");
    let row1_bal: String = r1.get(8);
    assert_eq!(row1_bal, "1.50");
    assert_eq!(r1.get::<String, _>(9), "h1");
    assert_eq!(r1.get::<Vec<u8>, _>(10), vec![1_u8, 2]);
    assert_eq!(r1.get::<Option<Vec<u8>>, _>(11), Some(vec![0xab_u8]));
    assert_eq!(
        r1.get::<chrono::NaiveDate, _>(12),
        NaiveDate::from_ymd_opt(1990, 1, 1).unwrap()
    );
    assert_eq!(
        r1.get::<chrono::DateTime<chrono::Utc>, _>(13),
        base_ts + chrono::Duration::seconds(1)
    );
    assert_eq!(
        r1.get::<Option<serde_json::Value>, _>(15),
        Some(serde_json::json!({ "row": 1 }))
    );
    assert_eq!(
        r1.get::<Option<String>, _>(16).as_deref(),
        Some("long-"),
        "row 1 bio truncated to VARCHAR(5)"
    );
    assert_eq!(r1.get::<i32, _>(17), 1_000);

    // Row 3 — NULL avatar_big.
    let r3 = &rows[2];
    assert_eq!(r3.get::<Option<Vec<u8>>, _>(11), None);

    // Row 4 — NULL meta.
    let r4 = &rows[3];
    assert_eq!(r4.get::<Option<serde_json::Value>, _>(15), None);

    // Row 5 — NULL bio source → `default = "n/a"`.
    let r5 = &rows[4];
    assert_eq!(
        r5.get::<Option<String>, _>(16).as_deref(),
        Some("n/a"),
        "NULL bio must fall back to default literal"
    );
    assert_eq!(r5.get::<i32, _>(17), 5_000);

    for (i, r) in rows.iter().enumerate() {
        let expected = (i + 1) as i64;
        assert_eq!(r.get::<i64, _>(0), expected);
    }

    // Cursor saved into the cockroach state table via `Storage::save_cursor`,
    // which goes through `with_serialization_retry` under Cockroach dialect.
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&cockroach.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "users");
    let parsed: air_elt_core::model::CursorState =
        serde_json::from_value(cursors[0].1.clone()).unwrap();
    assert_eq!(parsed.fields[0].value, Value::Int64(5));

    pg.pool.close().await;
    cockroach.pool.close().await;
}
