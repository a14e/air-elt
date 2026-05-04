#![allow(clippy::unwrap_used)]
use air_elt_commons_testing::mysql::mysql_pool;
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::{DataType, Value};
use air_elt_sink_mysql::{MySqlSink, MySqlSinkConfig};
use chrono::{TimeZone, Utc};
use sqlx::Executor;

async fn seed_table(pool: &sqlx::MySqlPool) {
    pool.execute(
        "CREATE TABLE events (
            id BIGINT NOT NULL PRIMARY KEY,
            label VARCHAR(64) NOT NULL,
            at TIMESTAMP NOT NULL,
            payload JSON
        ) ENGINE=InnoDB",
    )
    .await
    .expect("create events");
}

#[tokio::test]
async fn write_batch_and_validate_access() {
    let handle = mysql_pool().await;
    seed_table(&handle.pool).await;

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
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
    assert_eq!(
        schema.find("label").unwrap().data_type,
        DataType::Text { size: Some(64) }
    );
    assert_eq!(schema.find("at").unwrap().data_type, DataType::Timestamp);
    assert_eq!(schema.find("payload").unwrap().data_type, DataType::Json);
    assert!(!schema.find("id").unwrap().nullable, "PK must be NOT NULL");
    assert!(
        schema.find("payload").unwrap().nullable,
        "JSON without NOT NULL must be nullable"
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
    let report = sink.write_batch(&spec, ctx, &batch).await.expect("write");
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
}

/// All-nullable columns, one `Value::Null` bound per `DataType`. Catches any
/// typed-NULL arm regression in `push_values` (the 13-arm match on `*dt`).
#[tokio::test]
async fn all_nulls_across_data_types() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE null_matrix (
                id BIGINT NOT NULL PRIMARY KEY,
                c_bool TINYINT(1),
                c_i16 SMALLINT,
                c_i32 INT,
                c_i64 BIGINT,
                c_f32 FLOAT,
                c_f64 DOUBLE,
                c_text VARCHAR(64),
                c_bytes VARBINARY(64),
                c_date DATE,
                c_ts TIMESTAMP NULL,
                c_json JSON
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create null_matrix");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
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
    let report = sink.write_batch(&spec, ctx, &batch).await.expect("write");
    assert_eq!(report.rows_written, 1);

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
        Option<chrono::NaiveDate>,
        Option<chrono::DateTime<Utc>>,
        Option<serde_json::Value>,
    );
    let probe2: VariedProbe = sqlx::query_as(
        "SELECT c_text, c_bytes, c_date, c_ts, c_json FROM null_matrix WHERE id = 1",
    )
    .fetch_one(&handle.pool)
    .await
    .expect("read back 2");
    assert!(probe2.0.is_none());
    assert!(probe2.1.is_none());
    assert!(probe2.2.is_none());
    assert!(probe2.3.is_none());
    assert!(probe2.4.is_none());
}

/// Non-null values for every MySQL-native `DataType` — counterpart to
/// `all_nulls_across_data_types`. UUID is excluded (MySQL has no native
/// type; cross-vendor flows route through Text/Bytes via the convert layer).
#[tokio::test]
async fn all_types_non_null_round_trip() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE all_vals (
                id BIGINT NOT NULL PRIMARY KEY,
                c_bool TINYINT(1) NOT NULL,
                c_i16 SMALLINT NOT NULL,
                c_i32 INT NOT NULL,
                c_i64 BIGINT NOT NULL,
                c_f32 FLOAT NOT NULL,
                c_f64 DOUBLE NOT NULL,
                c_text VARCHAR(64) NOT NULL,
                c_bytes VARBINARY(16) NOT NULL,
                c_date DATE NOT NULL,
                c_ts TIMESTAMP NOT NULL,
                c_json JSON NOT NULL
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create all_vals");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let columns: Vec<String> = vec![
        "id", "c_bool", "c_i16", "c_i32", "c_i64", "c_f32", "c_f64", "c_text", "c_bytes", "c_date",
        "c_ts", "c_json",
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
    let date = chrono::NaiveDate::from_ymd_opt(2026, 6, 15).unwrap();

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
            Value::Json(serde_json::json!({"k": [1, 2]})),
        ])],
        next_cursor: None,
    };

    let ctx = sink.build_context(&spec).await.expect("build_context");
    let report = sink.write_batch(&spec, ctx, &batch).await.expect("write");
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

    let (c_text, c_bytes, c_json): (String, Vec<u8>, serde_json::Value) =
        sqlx::query_as("SELECT c_text, c_bytes, c_json FROM all_vals WHERE id = 1")
            .fetch_one(&handle.pool)
            .await
            .unwrap();
    assert_eq!(c_text, "test-text");
    assert_eq!(c_bytes, vec![0xCA, 0xFE]);
    assert_eq!(c_json, serde_json::json!({"k": [1, 2]}));

    let (c_f32, c_f64, c_date, c_ts): (f32, f64, chrono::NaiveDate, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT c_f32, c_f64, c_date, c_ts FROM all_vals WHERE id = 1")
            .fetch_one(&handle.pool)
            .await
            .unwrap();
    assert_eq!(c_f32, std::f32::consts::PI);
    assert_eq!(c_f64, std::f64::consts::E);
    assert_eq!(c_date, date);
    assert_eq!(c_ts, ts);
}
