#![allow(clippy::unwrap_used)]
use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::{DataType, Value};
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use chrono::{NaiveDate, TimeZone, Utc};
use sqlx::Executor;
use uuid::Uuid;

async fn seed_table(pool: &sqlx::PgPool) {
    pool.execute(
        "CREATE TABLE events (
            id BIGINT PRIMARY KEY,
            label TEXT NOT NULL,
            at TIMESTAMPTZ NOT NULL,
            payload JSONB
        )",
    )
    .await
    .expect("create events");
}

#[tokio::test]
async fn write_batch_and_validate_access() {
    let handle = pg_pool().await;
    seed_table(&handle.pool).await;

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "label".into(), "at".into(), "payload".into()],
        table: format!("{}.events", handle.schema),
        conflict: None,
    };

    sink.validate_access(&spec).await.expect("validate_access");

    let schema = sink.describe_schema(&spec.table).await.expect("describe");
    assert_eq!(schema.find("id").unwrap().data_type, DataType::Int64);
    assert_eq!(schema.find("label").unwrap().data_type, DataType::text());
    assert_eq!(schema.find("at").unwrap().data_type, DataType::Timestamp);
    assert_eq!(schema.find("payload").unwrap().data_type, DataType::Json);
    assert!(!schema.find("id").unwrap().nullable, "PK must be NOT NULL");
    assert!(
        schema.find("payload").unwrap().nullable,
        "JSONB without NOT NULL must be nullable"
    );

    let now = Utc.with_ymd_and_hms(2026, 4, 22, 10, 0, 0).unwrap();
    let batch = Batch {
        rows: vec![
            CoreRow::upsert(vec![
                Value::Int64(1),
                Value::Text("one".into()),
                Value::Timestamp(now),
                Value::Json(serde_json::json!({"k": 1})),
            ]),
            CoreRow::upsert(vec![
                Value::Int64(2),
                Value::Text("two".into()),
                Value::Timestamp(now),
                Value::Null,
            ]),
        ],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write");
    assert_eq!(report.rows_written, 2);

    let rows: Vec<(i64, String, Option<serde_json::Value>)> =
        sqlx::query_as("SELECT id, label, payload FROM events ORDER BY id")
            .fetch_all(&handle.pool)
            .await
            .expect("read back");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0],
        (1, "one".into(), Some(serde_json::json!({"k": 1})))
    );
    assert_eq!(rows[1], (2, "two".into(), None));
    handle.pool.close().await;
}

/// All-nullable columns, one `Value::Null` bound per `DataType`. Catches any
/// typed-NULL arm regression in `push_values`.
#[tokio::test]
async fn all_nulls_across_data_types() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE null_matrix (
                id BIGINT PRIMARY KEY,
                c_bool BOOLEAN,
                c_i16 SMALLINT,
                c_i32 INTEGER,
                c_i64 BIGINT,
                c_f32 REAL,
                c_f64 DOUBLE PRECISION,
                c_text TEXT,
                c_bytes BYTEA,
                c_date DATE,
                c_ts TIMESTAMPTZ,
                c_uuid UUID,
                c_json JSONB
            )",
        )
        .await
        .expect("create null_matrix");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let columns: Vec<String> = vec![
        "id".into(),
        "c_bool".into(),
        "c_i16".into(),
        "c_i32".into(),
        "c_i64".into(),
        "c_f32".into(),
        "c_f64".into(),
        "c_text".into(),
        "c_bytes".into(),
        "c_date".into(),
        "c_ts".into(),
        "c_uuid".into(),
        "c_json".into(),
    ];
    let spec = WriteSpec {
        columns: columns.clone(),
        table: format!("{}.null_matrix", handle.schema),
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");

    let mut row = vec![Value::Int64(1)];
    row.extend(std::iter::repeat_n(Value::Null, columns.len() - 1));
    let batch = Batch {
        rows: vec![CoreRow::upsert(row)],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write");
    assert_eq!(report.rows_written, 1);

    // All NOT NULL arms decoded back as NULL. Separate query_as blocks keep
    // each tuple below clippy's type-complexity threshold.
    #[allow(clippy::type_complexity)]
    type NumericProbe = (
        i64,
        Option<bool>,
        Option<i16>,
        Option<i32>,
        Option<i64>,
        Option<f32>,
        Option<f64>,
    );
    let probe: NumericProbe = sqlx::query_as(
        "SELECT id, c_bool, c_i16, c_i32, c_i64, c_f32, c_f64 FROM null_matrix WHERE id = 1",
    )
    .fetch_one(&handle.pool)
    .await
    .expect("read back");
    assert_eq!(probe.0, 1);
    assert!(probe.1.is_none());
    assert!(probe.2.is_none());
    assert!(probe.3.is_none());
    assert!(probe.4.is_none());
    assert!(probe.5.is_none());
    assert!(probe.6.is_none());

    #[allow(clippy::type_complexity)]
    type VariedProbe = (
        Option<String>,
        Option<Vec<u8>>,
        Option<NaiveDate>,
        Option<chrono::DateTime<Utc>>,
        Option<Uuid>,
        Option<serde_json::Value>,
    );
    let probe2: VariedProbe = sqlx::query_as(
        "SELECT c_text, c_bytes, c_date, c_ts, c_uuid, c_json FROM null_matrix WHERE id = 1",
    )
    .fetch_one(&handle.pool)
    .await
    .expect("read back 2");
    assert!(probe2.0.is_none());
    assert!(probe2.1.is_none());
    assert!(probe2.2.is_none());
    assert!(probe2.3.is_none());
    assert!(probe2.4.is_none());
    assert!(probe2.5.is_none());
    handle.pool.close().await;
}

/// Non-null values for all 12 DataType variants — complementary to
/// `all_nulls_across_data_types` which covers the NULL path.
#[tokio::test]
async fn all_types_non_null_round_trip() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE all_vals (
                id BIGINT PRIMARY KEY,
                c_bool BOOLEAN NOT NULL,
                c_i16 SMALLINT NOT NULL,
                c_i32 INTEGER NOT NULL,
                c_i64 BIGINT NOT NULL,
                c_f32 REAL NOT NULL,
                c_f64 DOUBLE PRECISION NOT NULL,
                c_text TEXT NOT NULL,
                c_bytes BYTEA NOT NULL,
                c_date DATE NOT NULL,
                c_ts TIMESTAMPTZ NOT NULL,
                c_uuid UUID NOT NULL,
                c_json JSONB NOT NULL
            )",
        )
        .await
        .expect("create all_vals");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let columns: Vec<String> = vec![
        "id", "c_bool", "c_i16", "c_i32", "c_i64", "c_f32", "c_f64", "c_text", "c_bytes", "c_date",
        "c_ts", "c_uuid", "c_json",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let spec = WriteSpec {
        columns: columns.clone(),
        table: format!("{}.all_vals", handle.schema),
        conflict: None,
    };

    let ts = Utc.with_ymd_and_hms(2026, 6, 15, 12, 0, 0).unwrap();
    let date = NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();
    let uid = Uuid::parse_str("a1a2a3a4-b1b2-c1c2-d1d2-e1e2e3e4e5e6").unwrap();

    let batch = Batch {
        rows: vec![CoreRow::upsert(vec![
            Value::Int64(1),
            Value::Bool(true),
            Value::Int16(i16::MAX),
            Value::Int32(i32::MIN),
            Value::Int64(i64::MAX),
            Value::Float32(std::f32::consts::PI),
            Value::Float64(std::f64::consts::E),
            Value::Text("test-text".into()),
            Value::Bytes(vec![0xCA, 0xFE]),
            Value::Date(date),
            Value::Timestamp(ts),
            Value::Uuid(uid),
            Value::Json(serde_json::json!({"k": [1, 2]})),
        ])],
        next_cursor: None,
    };

    let ctx = sink.build_context(&spec).await.expect("build_context");
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write");
    assert_eq!(report.rows_written, 1);

    let (c_bool, c_i16, c_i32, c_i64): (bool, i16, i32, i64) =
        sqlx::query_as("SELECT c_bool, c_i16, c_i32, c_i64 FROM all_vals WHERE id = 1")
            .fetch_one(&handle.pool)
            .await
            .unwrap();
    assert!(c_bool);
    assert_eq!(c_i16, i16::MAX);
    assert_eq!(c_i32, i32::MIN);
    assert_eq!(c_i64, i64::MAX);

    let (c_text, c_bytes, c_uuid, c_json): (String, Vec<u8>, Uuid, serde_json::Value) =
        sqlx::query_as("SELECT c_text, c_bytes, c_uuid, c_json FROM all_vals WHERE id = 1")
            .fetch_one(&handle.pool)
            .await
            .unwrap();
    assert_eq!(c_text, "test-text");
    assert_eq!(c_bytes, vec![0xCA, 0xFE]);
    assert_eq!(c_uuid, uid);
    assert_eq!(c_json, serde_json::json!({"k": [1, 2]}));

    let (c_date, c_ts): (NaiveDate, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT c_date, c_ts FROM all_vals WHERE id = 1")
            .fetch_one(&handle.pool)
            .await
            .unwrap();
    assert_eq!(c_date, date);
    assert_eq!(c_ts, ts);
    handle.pool.close().await;
}

/// Smoke test against CockroachDB: create a table, insert several rows, read
/// them back. Asserts that the Postgres-compatible insert path works against
/// the cockroach wire protocol (plain INSERT, no conflict block).
#[tokio::test]
async fn cockroach_smoke_insert_and_read_back() {
    let handle = cockroach_pool().await;
    handle
        .pool
        .execute("CREATE TABLE smoke (id INT PRIMARY KEY, label TEXT NOT NULL)")
        .await
        .expect("create smoke");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_database(),
        dialect: Dialect::Cockroach,
        ..Default::default()
    })
    .await
    .expect("connect cockroach sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "label".into()],
        table: "smoke".into(),
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");

    let rows: Vec<CoreRow> = (1..=5_i64)
        .map(|i| CoreRow::upsert(vec![Value::Int64(i), Value::Text(format!("row-{i}"))]))
        .collect();
    let batch = Batch {
        rows,
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write");
    assert_eq!(report.rows_written, 5);

    let back: Vec<(i64, String)> = sqlx::query_as("SELECT id, label FROM smoke ORDER BY id")
        .fetch_all(&handle.pool)
        .await
        .expect("read back");
    assert_eq!(back.len(), 5);
    assert_eq!(back[0], (1, "row-1".into()));
    assert_eq!(back[4], (5, "row-5".into()));
    handle.pool.close().await;
}

/// IP types end-to-end: bind canonical Value::Ipv4 / Value::Ipv6 (which the
/// sink routes through IpNetwork host bits) and a PgInetValue (which keeps
/// the netmask) against a PG `inet` column, then read the rows back via
/// `host()` text projection.
#[tokio::test]
async fn ip_types_round_trip() {
    use air_elt_commons_pg::types::{PgInetType, PgInetValue};

    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE ip_vals (
                id BIGINT PRIMARY KEY,
                c_ip INET NOT NULL
            )",
        )
        .await
        .expect("create ip_vals");

    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let columns: Vec<String> = vec!["id".to_string(), "c_ip".to_string()];
    let spec = WriteSpec {
        columns,
        table: format!("{}.ip_vals", handle.schema),
        conflict: None,
    };
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // 1) Value::Ipv4 → INET column lands as 192.0.2.1/32.
    // 2) Value::Ipv6 → INET column lands as 2001:db8::1/128.
    // 3) Value::Custom(PgInetValue(192.0.2.0/24)) preserves the mask.
    let rows = vec![
        CoreRow::upsert(vec![
            Value::Int64(1),
            Value::Ipv4(std::net::Ipv4Addr::new(192, 0, 2, 1)),
        ]),
        CoreRow::upsert(vec![
            Value::Int64(2),
            Value::Ipv6("2001:db8::1".parse().unwrap()),
        ]),
        CoreRow::upsert(vec![
            Value::Int64(3),
            Value::Custom(Box::new(PgInetValue("192.0.2.0/24".parse().unwrap()))),
        ]),
    ];

    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows,
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write");
    assert_eq!(report.rows_written, 3);

    // Read back via `host()` / `text(inet)`.
    let back: Vec<(i64, String)> = sqlx::query_as("SELECT id, text(c_ip) FROM ip_vals ORDER BY id")
        .fetch_all(&handle.pool)
        .await
        .expect("read back");
    assert_eq!(back.len(), 3);
    assert_eq!(back[0], (1, "192.0.2.1/32".to_string()));
    assert_eq!(back[1], (2, "2001:db8::1/128".to_string()));
    assert_eq!(back[2], (3, "192.0.2.0/24".to_string()));

    // Silences the unused-import warning for PgInetType, which is the
    // documented canonical descriptor for the `inet` column even though
    // the sink path doesn't take it as input.
    let _ = PgInetType::KIND;

    handle.pool.close().await;
}
