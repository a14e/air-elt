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

    pg.pool
        .execute(format!("CREATE SCHEMA \"{src_schema}\"").as_str())
        .await
        .unwrap();
    pg.pool
        .execute(
            format!(
                "CREATE TABLE \"{src_schema}\".events (
                    ts        TIMESTAMPTZ PRIMARY KEY,
                    event_id  BIGINT NOT NULL,
                    payload   BYTEA NOT NULL
                )"
            )
            .as_str(),
        )
        .await
        .unwrap();

    let base = chrono::Utc.with_ymd_and_hms(2026, 5, 14, 12, 0, 0).unwrap();
    for i in 0_i64..3 {
        let ts = base + chrono::Duration::seconds(i);
        let payload: Vec<u8> = vec![i as u8, (i + 1) as u8, (i + 2) as u8];
        sqlx::query(&format!(
            "INSERT INTO \"{src_schema}\".events (ts, event_id, payload) VALUES ($1, $2, $3)"
        ))
        .bind(ts)
        .bind(i)
        .bind(payload)
        .execute(&pg.pool)
        .await
        .unwrap();
    }

    qdb.drop_table("events_qdb").await;
    qdb.exec(
        "CREATE TABLE events_qdb (
            ts        TIMESTAMP,
            event_id  LONG,
            payload   BINARY
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
  events:
    source: src
    sink: snk
    storage: st
    from: "{src_schema}.events"
    to: "events_qdb"
    batch-limit: 2

    mapping:
      - {{ from: ts, to: ts }}
      - {{ from: event_id, to: event_id }}
      - {{ from: payload, to: payload }}

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

    // Read back rows and assert the event_id sequence + one payload byte
    // sequence matches what was inserted on the PG side.
    let rows = sqlx::query("SELECT event_id, payload FROM events_qdb ORDER BY ts ASC")
        .fetch_all(&qdb.pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 3);
    for (i, row) in rows.iter().enumerate() {
        let id: i64 = row.try_get("event_id").unwrap();
        assert_eq!(id, i as i64);
    }
    let first_payload: Vec<u8> = rows[0].try_get("payload").unwrap();
    assert_eq!(first_payload, vec![0_u8, 1, 2]);

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
