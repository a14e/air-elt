//! Cross-engine: PostgreSQL source → ClickHouse sink, PostgreSQL storage.
//!
//! Exercises the full pipeline end-to-end:
//!   * Source reads rows from a PG table via cursor,
//!   * Transform maps columns,
//!   * Sink writes rows to ClickHouse via RowBinary HTTP INSERT,
//!   * Cursor state is persisted in PG storage.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_commons_testing::pg::pg_pool;
use chrono::TimeZone;
use sqlx::Executor;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_clickhouse_round_trip() {
    let pg = pg_pool().await;
    let ch = clickhouse_handle().await;

    let src_schema = format!("{}_src", pg.schema);

    // --- PG source table ---
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".events (
                    id            BIGINT PRIMARY KEY,
                    name          TEXT NOT NULL,
                    count         INTEGER NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    for i in 1_i64..=5 {
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".events (id, name, count) VALUES ($1, $2, $3)"
        ))
        .bind(i)
        .bind(format!("event_{i}"))
        .bind((i * 10) as i32)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    // --- CH sink table ---
    ch.exec(
        "CREATE TABLE events (
            id Int64,
            name String,
            count Int32
        ) ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .unwrap();

    let pg_url = pg.url_with_search_path();
    let ch_url = &ch.url;

    #[allow(unsafe_code)]
    // Why: set_var is unsafe in edition 2024 due to potential read races.
    // Single-threaded test setup, no concurrent readers.
    unsafe {
        std::env::set_var("AIR_ELT_TEST_CH_URL", ch_url);
    }

    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: clickhouse
    config:
      url: env('AIR_ELT_TEST_CH_URL')
      database: "{ch_db}"
      user: "default"
      password: ""

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
    to: "events"
    batch-limit: 3

    mapping:
      id: id
      name: name
      count: count

    cursor:
      fields: [id]
      order: asc
      interval: "100ms"
"#,
        ch_db = ch.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // --- Verify data landed in ClickHouse ---
    let body = ch
        .exec("SELECT id, name, count FROM events ORDER BY id FORMAT TabSeparated")
        .await
        .unwrap();
    let rows: Vec<&str> = body.trim().split('\n').collect();
    assert_eq!(rows.len(), 5, "expected 5 rows in CH: {body}");
    for (i, row) in rows.iter().enumerate() {
        let cells: Vec<&str> = row.split('\t').collect();
        let expected_id = (i + 1).to_string();
        assert_eq!(cells[0], expected_id, "row {i}: id mismatch");
        assert_eq!(
            cells[1],
            format!("event_{expected_id}"),
            "row {i}: name mismatch"
        );
        let expected_count = ((i + 1) * 10).to_string();
        assert_eq!(cells[2], expected_count, "row {i}: count mismatch");
    }

    // --- Verify cursor was saved ---
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "events");

    pg.pool.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_clickhouse_wide_types() {
    let pg = pg_pool().await;
    let ch = clickhouse_handle().await;

    let src_schema = format!("{}_wide", pg.schema);

    // --- PG source table with many types ---
    // Exercises the broadest set of canonical types that PG can produce and
    // CH can consume:
    //   * Bool, Int16/32/64, Float32/64,
    //   * BigInt (numeric(20, 0)) — wider than i64,
    //   * Decimal(10, 2) — fractional decimal,
    //   * Text bounded (varchar) + unbounded (text),
    //   * Date, Timestamp (UTC), Uuid, Json (jsonb).
    //   * Nullable column to surface the BSON/Nullable contract.
    //   * Two truncate cases (text unbounded → varchar / Int64 → Int32).
    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".data (
                    id            BIGINT PRIMARY KEY,
                    flag          BOOLEAN NOT NULL,
                    small         SMALLINT NOT NULL,
                    measure_real  REAL NOT NULL,
                    measure_dbl   DOUBLE PRECISION NOT NULL,
                    label         VARCHAR(100) NOT NULL,
                    description   TEXT NOT NULL,
                    collected_at  TIMESTAMPTZ NOT NULL,
                    valid_from    DATE NOT NULL,
                    uid           UUID NOT NULL,
                    price         NUMERIC(10, 2) NOT NULL,
                    huge_count    NUMERIC(20, 0) NOT NULL,
                    nickname      TEXT,
                    payload       JSONB NOT NULL,
                    long_note     TEXT NOT NULL,
                    legacy_big    BIGINT NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let uid = uuid::Uuid::parse_str("a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11").unwrap();
    let collected_at = chrono::Utc
        .with_ymd_and_hms(2025, 6, 15, 12, 30, 45)
        .unwrap();
    let valid_from = chrono::NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
    let price: bigdecimal::BigDecimal = "123.45".parse().unwrap();
    let huge_count: bigdecimal::BigDecimal = "12345678901234567890".parse().unwrap();

    sqlx::query(&format!(
        "INSERT INTO \"{src_schema}\".data \
         (id, flag, small, measure_real, measure_dbl, label, description, \
          collected_at, valid_from, uid, price, huge_count, nickname, \
          payload, long_note, legacy_big) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"
    ))
    .bind(1_i64)
    .bind(true)
    .bind(42_i16)
    .bind(5.25_f32)
    .bind(7.125_f64)
    .bind("hello world")
    .bind("a longer plain text body")
    .bind(collected_at)
    .bind(valid_from)
    .bind(uid)
    .bind(price)
    .bind(huge_count.clone())
    .bind(Some("alice"))
    .bind(serde_json::json!({ "k": "v", "n": 1 }))
    .bind("alphabet-soup-overflow")
    .bind(10_000_i64)
    .execute(&pg.pool)
    .await
    .unwrap();

    // Second row exercises NULL on the nullable `nickname` column and
    // boundary values on the truncate paths.
    sqlx::query(&format!(
        "INSERT INTO \"{src_schema}\".data \
         (id, flag, small, measure_real, measure_dbl, label, description, \
          collected_at, valid_from, uid, price, huge_count, nickname, \
          payload, long_note, legacy_big) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)"
    ))
    .bind(2_i64)
    .bind(false)
    .bind(-7_i16)
    .bind(-1.0_f32)
    .bind(0.0_f64)
    .bind("plain")
    .bind("")
    .bind(collected_at + chrono::Duration::seconds(60))
    .bind(chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap())
    .bind(uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap())
    .bind("0.00".parse::<bigdecimal::BigDecimal>().unwrap())
    .bind("1".parse::<bigdecimal::BigDecimal>().unwrap())
    .bind(None::<String>)
    .bind(serde_json::json!([1, 2, 3]))
    .bind("tiny")
    .bind(-1_000_000_i64)
    .execute(&pg.pool)
    .await
    .unwrap();

    // --- CH sink table ---
    // Most columns mirror the source. `long_note_clipped` and `legacy_big`
    // are intentionally narrower to exercise the truncate path.
    ch.exec(
        "CREATE TABLE data_wide (
            id Int64,
            flag Bool,
            small Int16,
            measure_real Float32,
            measure_dbl Float64,
            label String,
            description String,
            collected_at DateTime,
            valid_from Date,
            uid UUID,
            price Decimal(10, 2),
            huge_count Decimal(20, 0),
            nickname Nullable(String),
            payload String,
            long_note_clipped String,
            legacy_big Int32
        ) ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .unwrap();

    let pg_url = pg.url_with_search_path();
    let ch_url = &ch.url;

    // Mapping:
    //   * identity for everything that lines up;
    //   * `payload = { from = "payload", truncate = true }` — JSON → String
    //     is a narrowing canonical path (Json → Text(n)) requiring opt-in;
    //   * `long_note_clipped = { from = "long_note", truncate = true }` —
    //     Text unbounded → bounded CH String is lossless but renaming to a
    //     differently-named sink column requires the long form;
    //   * `legacy_big = { from = "legacy_big", truncate = true }` — Int64
    //     → Int32 narrowing; values fit i32 so no overflow.
    let config_yaml = format!(
        r#"
sources:
  - name: src
    type: postgres
    config:
      url: "{pg_url}"

sinks:
  - name: snk
    type: clickhouse
    config:
      url: "{ch_url}"
      database: "{ch_db}"
      user: "default"
      password: ""

storages:
  - name: st
    type: postgres
    config:
      url: "{pg_url}"

flow:
  wide-data:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.data"
    to: "data_wide"
    batch-limit: 2

    mapping:
      id: id
      flag: flag
      small: small
      measure_real: measure_real
      measure_dbl: measure_dbl
      label: label
      description: description
      collected_at: collected_at
      valid_from: valid_from
      uid: uid
      price: price
      huge_count: huge_count
      nickname: nickname
      payload: {{ from: payload, truncate: true }}
      long_note_clipped: {{ from: long_note, truncate: true }}
      legacy_big: {{ from: legacy_big, truncate: true }}

    cursor:
      fields: [id]
      order: asc
      interval: "100ms"
"#,
        ch_db = ch.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.yml");
    std::fs::write(&config_path, &config_yaml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // --- Verify data landed in ClickHouse ---
    let body = ch
        .exec(
            "SELECT id, flag, small, measure_real, measure_dbl, label, description, \
                    collected_at, valid_from, uid, price, huge_count, \
                    coalesce(nickname, '<<NULL>>'), payload, long_note_clipped, legacy_big \
             FROM data_wide ORDER BY id FORMAT TabSeparated",
        )
        .await
        .unwrap();
    let rows: Vec<&str> = body.trim().split('\n').collect();
    assert_eq!(rows.len(), 2, "expected 2 rows in CH: {body}");

    // Row 1.
    let cells: Vec<&str> = rows[0].split('\t').collect();
    assert_eq!(cells.len(), 16, "expected 16 columns: {rows:?}");
    assert_eq!(cells[0], "1", "id");
    assert_eq!(cells[1], "true", "flag");
    assert_eq!(cells[2], "42", "small");
    let real_val: f32 = cells[3].parse().unwrap();
    assert!(
        (real_val - 5.25_f32).abs() < 0.01,
        "measure_real: {real_val}"
    );
    let dbl_val: f64 = cells[4].parse().unwrap();
    assert!(
        (dbl_val - 7.125_f64).abs() < 0.0001,
        "measure_dbl: {dbl_val}"
    );
    assert_eq!(cells[5], "hello world", "label");
    assert_eq!(cells[6], "a longer plain text body", "description");
    assert_eq!(cells[7], "2025-06-15 12:30:45", "collected_at");
    assert_eq!(cells[8], "2025-01-01", "valid_from");
    assert_eq!(cells[9], "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11", "uid");
    assert_eq!(cells[10], "123.45", "price");
    assert_eq!(cells[11], "12345678901234567890", "huge_count");
    assert_eq!(cells[12], "alice", "nickname row 1");
    // JSON serialised as canonical text by the runner.
    let payload_parsed: serde_json::Value = serde_json::from_str(cells[13]).unwrap();
    assert_eq!(payload_parsed, serde_json::json!({ "k": "v", "n": 1 }));
    assert_eq!(
        cells[14], "alphabet-soup-overflow",
        "long_note round-trips intact (CH String is unbounded)"
    );
    assert_eq!(cells[15], "10000", "legacy_big row 1 fits i32");

    // Row 2 — exercises NULL nickname + boundary values.
    let cells2: Vec<&str> = rows[1].split('\t').collect();
    assert_eq!(cells2.len(), 16);
    assert_eq!(cells2[0], "2");
    assert_eq!(cells2[1], "false");
    assert_eq!(cells2[2], "-7");
    assert_eq!(cells2[6], "", "empty description must round-trip");
    // CH FORMAT TabSeparated may drop trailing zeros on Decimal — assert
    // the numeric equality rather than the textual form.
    let price2: bigdecimal::BigDecimal = cells2[10].parse().unwrap();
    assert_eq!(price2, "0".parse::<bigdecimal::BigDecimal>().unwrap());
    assert_eq!(cells2[11], "1", "BigInt 1");
    assert_eq!(
        cells2[12], "<<NULL>>",
        "NULL nickname must land as SQL NULL on the CH side"
    );
    let payload2: serde_json::Value = serde_json::from_str(cells2[13]).unwrap();
    assert_eq!(payload2, serde_json::json!([1, 2, 3]));
    assert_eq!(cells2[14], "tiny");
    assert_eq!(cells2[15], "-1000000");

    // --- Verify cursor was saved ---
    let cursors: Vec<(String, serde_json::Value)> =
        sqlx::query_as("SELECT flow, state FROM air_elt_cursors")
            .fetch_all(&pg.pool)
            .await
            .unwrap();
    assert_eq!(cursors.len(), 1);
    assert_eq!(cursors[0].0, "wide-data");

    pg.pool.close().await;
}

/// PG → CH IPv4 / IPv6 round-trip. PG side carries an `inet` column
/// with both v4 and v6 host addresses; the runner narrows
/// `postgresql.inet → Ipv4/Ipv6` under per-column `truncate = true`
/// and the CH sink encodes via RowBinary (`Ipv4` LE u32, `Ipv6` 16
/// BE octets). The CH side stores them in dedicated `IPv4` / `IPv6`
/// columns — separate rows because PG `inet` is a single column
/// carrying either family.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pg_to_clickhouse_ip_round_trip() {
    let pg = pg_pool().await;
    let ch = clickhouse_handle().await;

    let src_schema = format!("{}_ip", pg.schema);

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".v4_rows (
                    id BIGINT PRIMARY KEY,
                    addr INET NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".v6_rows (
                    id BIGINT PRIMARY KEY,
                    addr INET NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "INSERT INTO \"{src_schema}\".v4_rows(id, addr) VALUES \
                 (1, '192.0.2.1'::inet), (2, '203.0.113.42'::inet)"
            )
            .as_str(),
        )
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "INSERT INTO \"{src_schema}\".v6_rows(id, addr) VALUES \
                 (1, '2001:db8::1'::inet), (2, 'fe80::1'::inet)"
            )
            .as_str(),
        )
        .await
        .unwrap();

    ch.exec("CREATE TABLE v4_rows (id Int64, addr IPv4) ENGINE = MergeTree() ORDER BY id")
        .await
        .unwrap();
    ch.exec("CREATE TABLE v6_rows (id Int64, addr IPv6) ENGINE = MergeTree() ORDER BY id")
        .await
        .unwrap();

    let pg_url = pg.url_with_search_path();
    let ch_url = &ch.url;
    let config_toml = format!(
        r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = {{ url = "{pg_url}" }}

[[sinks]]
name = "ch_sink"
type = "clickhouse"
config = {{ url = "{ch_url}", database = "{ch_db}", user = "default", password = "" }}

[[storages]]
name = "pg_state"
type = "postgres"
config = {{ url = "{pg_url}" }}

[flow.v4]
source = "pg_src"
sink = "ch_sink"
storage = "pg_state"
from = "{src_schema}.v4_rows"
to = "v4_rows"
batch-limit = 8
cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
[flow.v4.mapping]
id = "id"
addr = {{ from = "addr", truncate = true }}

[flow.v6]
source = "pg_src"
sink = "ch_sink"
storage = "pg_state"
from = "{src_schema}.v6_rows"
to = "v6_rows"
batch-limit = 8
cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
[flow.v6.mapping]
id = "id"
addr = {{ from = "addr", truncate = true }}
"#,
        ch_db = ch.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, &config_toml).unwrap();
    let app = App::from_path(&path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let v4_out = ch
        .exec("SELECT id, toString(addr) FROM v4_rows ORDER BY id FORMAT TabSeparated")
        .await
        .unwrap();
    let v4_lines: Vec<&str> = v4_out.trim().split('\n').collect();
    assert_eq!(v4_lines, vec!["1\t192.0.2.1", "2\t203.0.113.42"]);

    let v6_out = ch
        .exec("SELECT id, toString(addr) FROM v6_rows ORDER BY id FORMAT TabSeparated")
        .await
        .unwrap();
    let v6_lines: Vec<&str> = v6_out.trim().split('\n').collect();
    assert_eq!(v6_lines, vec!["1\t2001:db8::1", "2\tfe80::1"]);

    pg.pool.close().await;
}
