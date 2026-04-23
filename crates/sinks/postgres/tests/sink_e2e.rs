#![allow(clippy::unwrap_used)]
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
    };

    sink.validate_access(&spec).await.expect("validate_access");

    let schema = sink.describe_schema(&spec.table).await.expect("describe");
    assert_eq!(schema.find("id").unwrap().data_type, DataType::Int64);
    assert_eq!(schema.find("label").unwrap().data_type, DataType::Text);
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
            CoreRow {
                values: vec![
                    Value::Int64(1),
                    Value::Text("one".into()),
                    Value::Timestamp(now),
                    Value::Json(serde_json::json!({"k": 1})),
                ],
            },
            CoreRow {
                values: vec![
                    Value::Int64(2),
                    Value::Text("two".into()),
                    Value::Timestamp(now),
                    Value::Null,
                ],
            },
        ],
        next_cursor: None,
    };
    let report = sink.write_batch(&spec, &batch).await.expect("write");
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
    };
    sink.validate_access(&spec).await.expect("validate_access");

    let mut row = vec![Value::Int64(1)];
    row.extend(std::iter::repeat_n(Value::Null, columns.len() - 1));
    let batch = Batch {
        rows: vec![CoreRow { values: row }],
        next_cursor: None,
    };
    let report = sink.write_batch(&spec, &batch).await.expect("write");
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
}
