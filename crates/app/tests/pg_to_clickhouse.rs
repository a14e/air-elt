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
      - {{ from: id, to: id }}
      - {{ from: name, to: name }}
      - {{ from: count, to: count }}

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
                    collected_at  TIMESTAMPTZ NOT NULL,
                    valid_from    DATE NOT NULL,
                    uid           UUID NOT NULL,
                    price         NUMERIC(10,2) NOT NULL
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

    sqlx::query(&format!(
        "INSERT INTO \"{src_schema}\".data \
         (id, flag, small, measure_real, measure_dbl, label, collected_at, valid_from, uid, price) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)"
    ))
    .bind(1_i64)
    .bind(true)
    .bind(42_i16)
    .bind(5.25_f32)
    .bind(7.125_f64)
    .bind("hello world")
    .bind(collected_at)
    .bind(valid_from)
    .bind(uid)
    .bind(price)
    .execute(&pg.pool)
    .await
    .unwrap();

    // --- CH sink table ---
    ch.exec(
        "CREATE TABLE data_wide (
            id Int64,
            flag Bool,
            small Int16,
            measure_real Float32,
            measure_dbl Float64,
            label String,
            collected_at DateTime,
            valid_from Date,
            uid UUID,
            price Decimal(10, 2)
        ) ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .unwrap();

    let pg_url = pg.url_with_search_path();
    let ch_url = &ch.url;

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
    batch-limit: 1

    mapping:
      - {{ from: id, to: id }}
      - {{ from: flag, to: flag }}
      - {{ from: small, to: small }}
      - {{ from: measure_real, to: measure_real }}
      - {{ from: measure_dbl, to: measure_dbl }}
      - {{ from: label, to: label }}
      - {{ from: collected_at, to: collected_at }}
      - {{ from: valid_from, to: valid_from }}
      - {{ from: uid, to: uid }}
      - {{ from: price, to: price }}

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
        .exec("SELECT id, flag, small, measure_real, measure_dbl, label, collected_at, valid_from, uid, price FROM data_wide ORDER BY id FORMAT TabSeparated")
        .await
        .unwrap();
    let rows: Vec<&str> = body.trim().split('\n').collect();
    assert_eq!(rows.len(), 1, "expected 1 row in CH: {body}");
    let cells: Vec<&str> = rows[0].split('\t').collect();
    assert_eq!(cells.len(), 10, "expected 10 columns: {rows:?}");

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
    assert_eq!(cells[6], "2025-06-15 12:30:45", "collected_at");
    assert_eq!(cells[7], "2025-01-01", "valid_from");
    assert_eq!(cells[8], "a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11", "uid");
    assert_eq!(cells[9], "123.45", "price");

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
