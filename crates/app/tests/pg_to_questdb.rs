//! Cross-engine: PostgreSQL source → QuestDB sink, PostgreSQL storage.
//!
//! Exercises the full pipeline:
//!   * PG source reads rows by cursor,
//!   * Transform maps columns,
//!   * QuestDB sink writes via pg-wire (the only transport),
//!   * Cursor state is persisted in PG storage.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_commons_testing::questdb::questdb_pool;
use chrono::TimeZone;
use sqlx::{Executor, Row};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_questdb() {
    let pg = pg_pool().await;
    let qdb = questdb_pool().await.expect("questdb pool");

    let src_schema = format!("{}_qdb", pg.schema);

    // Source carries the broadest type set the QuestDB sink will accept.
    // Skipped (per QuestDB's `type_supported`):
    //   * BigInt / Decimal — QuestDB has no arbitrary-precision numeric.
    //   * XML — no XML column type.
    //   * UInt* — PG never produces them anyway.
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".events (
                    ts          TIMESTAMPTZ PRIMARY KEY,
                    event_id    BIGINT NOT NULL,
                    payload     BYTEA NOT NULL,
                    is_active   BOOLEAN NOT NULL,
                    rating      SMALLINT NOT NULL,
                    seq32       INT NOT NULL,
                    score32     REAL NOT NULL,
                    score64     DOUBLE PRECISION NOT NULL,
                    label       VARCHAR(64) NOT NULL,
                    description TEXT NOT NULL,
                    born_on     DATE NOT NULL,
                    public_id   UUID NOT NULL,
                    meta        JSONB NOT NULL,
                    bio         TEXT,
                    legacy_big  BIGINT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = chrono::Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap();
    let uid_template = uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a00").unwrap();
    for i in 0_i64..3 {
        let ts = base + chrono::Duration::seconds(i);
        let payload: Vec<u8> = vec![i as u8, (i + 1) as u8, (i + 2) as u8];
        let uid = uuid::Uuid::from_u128(uid_template.as_u128() + i as u128);
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".events (
                ts, event_id, payload, is_active, rating, seq32, score32, score64,
                label, description, born_on, public_id, meta, bio, legacy_big
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8,
                $9, $10, $11, $12, $13, $14, $15
            )"
        ))
        .bind(ts)
        .bind(i)
        .bind(payload)
        .bind(i % 2 == 0)
        .bind(i as i16 + 10)
        .bind(i as i32 * 100)
        .bind(0.5_f32 + i as f32)
        .bind(0.125_f64 * (i + 1) as f64)
        .bind(format!("label-{i}"))
        .bind(format!("description for row {i}"))
        .bind(chrono::NaiveDate::from_ymd_opt(2020, 1, i as u32 + 1).unwrap())
        .bind(uid)
        .bind(serde_json::json!({ "row": i, "kind": "demo" }))
        // Row 0: longer than VARCHAR(5) sink → truncate path.
        // Row 1: NULL → default kicks in.
        // Row 2: short value → passthrough under truncate.
        .bind(match i {
            0 => Some("alphabet-soup".to_string()),
            1 => None,
            _ => Some("hi".to_string()),
        })
        // Row values fit i32 — exercises Int64 → Int32 truncate cleanly.
        .bind(1_000_i64 + i)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    qdb.drop_table("events_qdb").await;
    qdb.exec(
        "CREATE TABLE events_qdb (
            ts          TIMESTAMP,
            event_id    LONG,
            payload     BINARY,
            is_active   BOOLEAN,
            rating      BYTE,
            seq32       INT,
            score32     FLOAT,
            score64     DOUBLE,
            label       VARCHAR,
            description STRING,
            born_on     DATE,
            public_id   UUID,
            meta        STRING,
            bio         VARCHAR,
            legacy_big  INT
        ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .unwrap();

    let pg_url = pg.url_with_search_path();
    let qdb_url = &qdb.url;

    // Mapping notes:
    //   * `rating` SMALLINT (Int16) → BYTE (Int8) — narrowing, truncate=true.
    //   * `meta` JSONB → STRING is Json → Text(unbounded), allowed because
    //     QuestDB STRING is unbounded; truncate=true keeps the matrix happy
    //     for the Json → Text narrowing path.
    //   * `bio` source is nullable; default fires on NULL.
    //   * `legacy_big` BIGINT → INT — narrowing with truncate, values fit i32.
    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: questdb
    config:
      url: "{qdb_url}"

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"

flow:
  events:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.events"
    to: "events_qdb"
    batch-limit: 2

    mapping:
      ts: ts
      event_id: event_id
      payload: payload
      is_active: is_active
      rating: {{ from: rating, truncate: true }}
      seq32: seq32
      score32: score32
      score64: score64
      label: label
      description: description
      born_on: born_on
      public_id: public_id
      meta: {{ from: meta, truncate: true }}
      bio: {{ from: bio, truncate: true, default: "n/a" }}
      legacy_big: {{ from: legacy_big, truncate: true }}

    cursor:
      fields: [ts]
      order: asc
      interval: "100ms"
"#
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // QuestDB WAL-applies pg-wire INSERTs asynchronously; poll for the
    // expected row count up to 5s before giving up.
    let mut row_count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS n FROM events_qdb")
            .fetch_one(&qdb.pool)
            .await
            .unwrap();
        row_count = row.try_get::<i64, _>("n").expect("count decode");
        if row_count == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(row_count, 3, "expected 3 rows in QuestDB");

    // Read back rows and assert every column round-trips.
    let rows = sqlx::query(
        "SELECT event_id, payload, is_active, rating, seq32, score32, score64, \
                label, description, born_on, public_id, meta, bio, legacy_big \
         FROM events_qdb ORDER BY ts ASC",
    )
    .fetch_all(&qdb.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 3);

    // Row 0 — `bio` truncates from "alphabet-soup" (length > 5)?
    // QuestDB VARCHAR is unbounded by default; without an explicit length
    // limit truncation does not kick in. The truncate flag is still valid
    // (matrix lossless coverage); the round-tripped value is the full
    // source text.
    let r0 = &rows[0];
    assert_eq!(r0.get::<i64, _>("event_id"), 0);
    assert_eq!(r0.get::<Vec<u8>, _>("payload"), vec![0_u8, 1, 2]);
    assert!(r0.get::<bool, _>("is_active"));
    assert_eq!(r0.get::<i16, _>("rating"), 10);
    assert_eq!(r0.get::<i32, _>("seq32"), 0);
    assert!((r0.get::<f32, _>("score32") - 0.5_f32).abs() < f32::EPSILON);
    assert!((r0.get::<f64, _>("score64") - 0.125_f64).abs() < f64::EPSILON);
    assert_eq!(r0.get::<String, _>("label"), "label-0");
    assert_eq!(r0.get::<String, _>("description"), "description for row 0");
    // QuestDB's DATE type stores milliseconds since epoch and surfaces as a
    // TIMESTAMP over pg-wire (not a naive date). Decode as `NaiveDateTime`
    // and assert the calendar-day component matches.
    let born_dt: chrono::NaiveDateTime = r0.get("born_on");
    assert_eq!(
        born_dt.date(),
        chrono::NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()
    );
    assert_eq!(r0.get::<uuid::Uuid, _>("public_id"), uid_template);
    let meta_raw: String = r0.get("meta");
    let meta: serde_json::Value = serde_json::from_str(&meta_raw).unwrap();
    assert_eq!(meta, serde_json::json!({ "row": 0, "kind": "demo" }));
    assert_eq!(
        r0.get::<Option<String>, _>("bio").as_deref(),
        Some("alphabet-soup")
    );
    assert_eq!(r0.get::<i32, _>("legacy_big"), 1_000);

    // Row 1 — bio source is NULL → default "n/a".
    let r1 = &rows[1];
    assert_eq!(r1.get::<i64, _>("event_id"), 1);
    assert_eq!(r1.get::<i16, _>("rating"), 11);
    assert_eq!(
        r1.get::<Option<String>, _>("bio").as_deref(),
        Some("n/a"),
        "NULL bio must fall back to default literal"
    );
    assert_eq!(r1.get::<i32, _>("legacy_big"), 1_001);

    // Row 2 — short bio passes through under truncate.
    let r2 = &rows[2];
    assert_eq!(r2.get::<i64, _>("event_id"), 2);
    assert_eq!(r2.get::<i16, _>("rating"), 12);
    assert_eq!(r2.get::<Option<String>, _>("bio").as_deref(), Some("hi"));
    assert_eq!(r2.get::<i32, _>("legacy_big"), 1_002);

    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "events");

    qdb.drop_table("events_qdb").await;
    qdb.pool.close().await;
    pg.pool.close().await;
}

/// PG `inet` (IPv4 only) → QuestDB `IPV4` column. The pipeline
/// uses `truncate = true` to narrow `postgresql.inet → Ipv4` (drops
/// the implicit /32 mask). QuestDB has no IPv6 column type, so only
/// v4 is exercised here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_questdb_ip_round_trip() {
    let pg = pg_pool().await;
    let qdb = questdb_pool().await.expect("questdb pool");

    let src_schema = format!("{}_qdb_ip", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".v4_rows (
                    ts   TIMESTAMPTZ PRIMARY KEY,
                    addr INET NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();
    let base = chrono::Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap();
    for (i, ip) in ["192.0.2.1", "203.0.113.42", "10.0.0.1"].iter().enumerate() {
        let ts = base + chrono::Duration::seconds(i as i64);
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".v4_rows(ts, addr) VALUES ($1, $2::inet)"
        ))
        .bind(ts)
        .bind(*ip)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    qdb.drop_table("v4_rows_qdb").await;
    qdb.exec(
        "CREATE TABLE v4_rows_qdb (\
            ts TIMESTAMP, \
            addr IPV4\
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .expect("create");

    let pg_url = pg.url_with_search_path();
    let qdb_url = qdb.url.clone();
    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "qdb_sink"
type = "questdb"
config = {{ url = "{qdb_url}" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.v4]
source = "pg_src"
sink = "qdb_sink"
storage = "pg_state"
from = "{src_schema}.v4_rows"
to = "v4_rows_qdb"
batch-limit = 8
cursor = {{ fields = ["ts"], order = "asc", interval = "100ms" }}
[flow.v4.mapping]
ts = "ts"
addr = {{ from = "addr", truncate = true }}
"#
    );

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, &config_toml).unwrap();
    let app = App::from_path(&path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // Poll until QuestDB's async insert lands all 3 rows.
    let mut got: Vec<(String,)> = Vec::new();
    for _ in 0..50 {
        let rows: Vec<sqlx::postgres::PgRow> =
            sqlx::query("SELECT addr::string AS s FROM v4_rows_qdb ORDER BY ts ASC")
                .fetch_all(&qdb.pool)
                .await
                .expect("select");
        if rows.len() == 3 {
            got = rows
                .iter()
                .map(|r| (r.try_get::<String, _>("s").expect("addr text"),))
                .collect();
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(
        got,
        vec![
            ("192.0.2.1".to_string(),),
            ("203.0.113.42".to_string(),),
            ("10.0.0.1".to_string(),),
        ]
    );

    pg.pool.close().await;
    qdb.pool.close().await;
}

/// Native QuestDB `DOUBLE[]` end-to-end (AIR-124). A computed array column
/// `[a, b, c]` over three `NOT NULL` `double precision` source columns
/// yields a non-null-element `Array<Float64>` — exactly what QuestDB's
/// `DOUBLE[]` accepts. (A PG `double precision[]` source column cannot
/// target it: PG always reports array elements as nullable, and the type
/// matrix forbids a nullable element landing in QuestDB's non-null
/// `DOUBLE[]`.) Exercises the expression-language array literal through the
/// Transform and the sink's `bind_double_array` pg-wire path live.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_questdb_double_array() {
    let pg = pg_pool().await;
    let qdb = questdb_pool().await.expect("questdb pool");

    let src_schema = format!("{}_qdb_arr", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".sensor (
                    ts TIMESTAMPTZ PRIMARY KEY,
                    a  DOUBLE PRECISION NOT NULL,
                    b  DOUBLE PRECISION NOT NULL,
                    c  DOUBLE PRECISION NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = chrono::Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap();
    for i in 0_i64..3 {
        let ts = base + chrono::Duration::seconds(i);
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".sensor (ts, a, b, c) VALUES ($1, $2, $3, $4)"
        ))
        .bind(ts)
        .bind(1.0_f64 + i as f64)
        .bind(2.0_f64 + i as f64)
        .bind(3.0_f64 + i as f64)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    qdb.drop_table("sensor_qdb").await;
    qdb.exec(
        "CREATE TABLE sensor_qdb (
            ts       TIMESTAMP,
            readings DOUBLE[]
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .unwrap();

    let pg_url = pg.url_with_search_path();
    let qdb_url = &qdb.url;
    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: questdb
    config:
      url: "{qdb_url}"

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"

flow:
  sensor:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.sensor"
    to: "sensor_qdb"
    batch-limit: 2

    mapping:
      ts: ts
      readings:
        compute: "[`a`, `b`, `c`]"

    cursor:
      fields: [ts]
      order: asc
      interval: "100ms"
"#
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let mut row_count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS n FROM sensor_qdb")
            .fetch_one(&qdb.pool)
            .await
            .unwrap();
        row_count = row.try_get::<i64, _>("n").expect("count decode");
        if row_count == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(row_count, 3, "expected 3 rows in QuestDB");

    let rows = sqlx::query("SELECT readings FROM sensor_qdb ORDER BY ts ASC")
        .fetch_all(&qdb.pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get::<Vec<f64>, _>("readings"), vec![1.0, 2.0, 3.0]);
    assert_eq!(rows[1].get::<Vec<f64>, _>("readings"), vec![2.0, 3.0, 4.0]);
    assert_eq!(rows[2].get::<Vec<f64>, _>("readings"), vec![3.0, 4.0, 5.0]);

    qdb.drop_table("sensor_qdb").await;
    qdb.pool.close().await;
    pg.pool.close().await;
}

/// Native PG `double precision[]` → QuestDB `DOUBLE[]` via `truncate=true`
/// (AIR-124 follow-up). PG always reports array elements as nullable and
/// QuestDB `DOUBLE[]` elements are non-null; `truncate` opts into dropping
/// the NULL members at conversion. A source array `[1.0, NULL, 3.0]` lands
/// as `[1.0, 3.0]`, and an all-NULL array collapses to empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_questdb_native_double_array_truncate() {
    let pg = pg_pool().await;
    let qdb = questdb_pool().await.expect("questdb pool");

    let src_schema = format!("{}_qdb_narr", pg.schema);
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".sensor (
                    ts       TIMESTAMPTZ PRIMARY KEY,
                    readings DOUBLE PRECISION[] NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = chrono::Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
    let samples: [Vec<Option<f64>>; 3] = [
        vec![Some(1.0), None, Some(3.0)], // NULL member dropped under truncate
        vec![Some(10.0), Some(20.0)],     // no nulls
        vec![None, None],                 // all-null → empty
    ];
    for (i, vals) in samples.iter().enumerate() {
        let ts = base + chrono::Duration::seconds(i as i64);
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".sensor (ts, readings) VALUES ($1, $2)"
        ))
        .bind(ts)
        .bind(vals)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    qdb.drop_table("sensor_qdb_narr").await;
    qdb.exec(
        "CREATE TABLE sensor_qdb_narr (
            ts       TIMESTAMP,
            readings DOUBLE[]
         ) TIMESTAMP(ts) PARTITION BY DAY;",
    )
    .await
    .unwrap();

    let pg_url = pg.url_with_search_path();
    let qdb_url = &qdb.url;
    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: questdb
    config:
      url: "{qdb_url}"

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"

flow:
  sensor:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.sensor"
    to: "sensor_qdb_narr"
    batch-limit: 2

    mapping:
      ts: ts
      readings:
        from: readings
        truncate: true

    cursor:
      fields: [ts]
      order: asc
      interval: "100ms"
"#
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let mut row_count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query("SELECT count() AS n FROM sensor_qdb_narr")
            .fetch_one(&qdb.pool)
            .await
            .unwrap();
        row_count = row.try_get::<i64, _>("n").expect("count decode");
        if row_count == 3 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(row_count, 3, "expected 3 rows in QuestDB");

    let rows = sqlx::query("SELECT readings FROM sensor_qdb_narr ORDER BY ts ASC")
        .fetch_all(&qdb.pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    // NULL members dropped under truncate; all-null collapses to empty.
    assert_eq!(rows[0].get::<Vec<f64>, _>("readings"), vec![1.0, 3.0]);
    assert_eq!(rows[1].get::<Vec<f64>, _>("readings"), vec![10.0, 20.0]);
    assert!(rows[2].get::<Vec<f64>, _>("readings").is_empty());

    qdb.drop_table("sensor_qdb_narr").await;
    qdb.pool.close().await;
    pg.pool.close().await;
}
