//! Regression for AIR-69 — twelve-column OLTP table shape borrowed from
//! the thousand-flows manual test. The original failure was
//!
//!   `inconvertible types: STRING -> BOOLEAN [from=$7, to=description]`
//!
//! surfaced by `validate_access` when the sink dry-run probe binds
//! `INSERT INTO t (...) SELECT $1,...,$N FROM long_sequence(0)` over
//! QuestDB's pg-wire surface. The repro covers every native type
//! involved (`LONG`, `SYMBOL` with and without capacity, `DOUBLE`,
//! `STRING`, `INT`, `BOOLEAN`, `TIMESTAMP`) and exercises both the
//! plain append-only DDL and the mutable `DEDUP UPSERT KEYS(...)`
//! variant — both must pass `validate_access` and round-trip a row.
//!
//! QuestDB pg-wire quirk: the planner refuses to coerce STRING-typed
//! bind parameters into BOOLEAN, INT, LONG, DOUBLE, TIMESTAMP columns
//! when the source expression is a bare `SELECT $1, ..., $N FROM
//! long_sequence(0)` — its inferred output schema collapses to a single
//! BOOLEAN row type before the column-list rewrite kicks in. The sink
//! therefore emits a `WHERE 1=0` form on a single-row VALUES seed
//! instead, which forces per-parameter type resolution against the
//! target table.

use chrono::{TimeZone, Utc};
use sqlx::Row as _;

use air_elt_commons_testing::questdb::questdb_pool;
use air_elt_core::model::{Batch, Row, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_questdb::{QuestDbSink, QuestDbSinkConfig};

const OLTP_COLUMNS: &[&str] = &[
    "id",
    "user_id",
    "email",
    "amount",
    "currency",
    "status",
    "description",
    "quantity",
    "is_active",
    "metadata",
    "created_at",
    "updated_at",
];

fn oltp_ddl(table: &str, wal: bool, dedup: bool) -> String {
    let wal_clause = if wal { " WAL" } else { "" };
    let dedup_clause = if dedup {
        " DEDUP UPSERT KEYS(updated_at, id)"
    } else {
        ""
    };
    format!(
        "CREATE TABLE {table} ( \
            id LONG, \
            user_id LONG, \
            email SYMBOL CAPACITY 1000, \
            amount DOUBLE, \
            currency SYMBOL CAPACITY 16, \
            status SYMBOL CAPACITY 16, \
            description STRING, \
            quantity INT, \
            is_active BOOLEAN, \
            metadata STRING, \
            created_at TIMESTAMP, \
            updated_at TIMESTAMP \
         ) TIMESTAMP(updated_at) PARTITION BY DAY{wal_clause}{dedup_clause};"
    )
}

fn oltp_write_spec(table: &str) -> WriteSpec {
    WriteSpec {
        table: table.to_string(),
        columns: OLTP_COLUMNS.iter().map(|c| (*c).to_string()).collect(),
        conflict: None,
    }
}

fn realistic_row() -> Row {
    let created = Utc
        .with_ymd_and_hms(2026, 1, 15, 9, 30, 0)
        .single()
        .expect("created_at");
    let updated = created + chrono::Duration::seconds(7);
    Row::upsert(vec![
        Value::Int64(42),
        Value::Int64(9001),
        Value::Text("buyer@example.com".to_string()),
        Value::Float64(12.5),
        Value::Text("USD".to_string()),
        Value::Text("paid".to_string()),
        Value::Text("first order".to_string()),
        Value::Int32(3),
        Value::Bool(true),
        Value::Text("{\"channel\":\"web\"}".to_string()),
        Value::Timestamp(created),
        Value::Timestamp(updated),
    ])
}

async fn poll_count(pool: &sqlx::PgPool, table: &str, expected: i64) -> i64 {
    let mut count: i64 = 0;
    for _ in 0..50 {
        let row = sqlx::query(&format!("SELECT count() AS c FROM {table}"))
            .fetch_one(pool)
            .await
            .expect("count");
        count = row.try_get::<i64, _>("c").expect("count decode");
        if count == expected {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    count
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn append_only_validate_access_passes() {
    let h = questdb_pool().await.expect("questdb pool");
    let table = "oltp_append";
    h.drop_table(table).await;
    h.exec(&oltp_ddl(table, true, false)).await.expect("create");

    let sink = QuestDbSink::connect(QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    })
    .await
    .expect("connect");

    sink.validate_access(&oltp_write_spec(table))
        .await
        .expect("validate_access on append-only OLTP table");

    h.drop_table(table).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mutable_validate_access_passes() {
    let h = questdb_pool().await.expect("questdb pool");
    let table = "oltp_mutable";
    h.drop_table(table).await;
    h.exec(&oltp_ddl(table, true, true)).await.expect("create");

    let sink = QuestDbSink::connect(QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    })
    .await
    .expect("connect");

    sink.validate_access(&oltp_write_spec(table))
        .await
        .expect("validate_access on DEDUP UPSERT mutable OLTP table");

    h.drop_table(table).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn oltp_row_round_trip() {
    let h = questdb_pool().await.expect("questdb pool");
    let table = "oltp_roundtrip";
    h.drop_table(table).await;
    h.exec(&oltp_ddl(table, true, false)).await.expect("create");

    let sink = QuestDbSink::connect(QuestDbSinkConfig {
        url: h.url.clone(),
        ..Default::default()
    })
    .await
    .expect("connect");
    let spec = oltp_write_spec(table);
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let row = realistic_row();
    let report = sink
        .write_batch(
            &spec,
            &ctx,
            Batch {
                rows: vec![row],
                next_cursor: None,
            },
            false,
        )
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written, 1);

    assert_eq!(poll_count(&h.pool, table, 1).await, 1);

    let row = sqlx::query(&format!(
        "SELECT id, user_id, email, amount, currency, status, description, \
                quantity, is_active, metadata, created_at, updated_at \
         FROM {table}"
    ))
    .fetch_one(&h.pool)
    .await
    .expect("select round-trip");

    let id: i64 = row.try_get("id").expect("id");
    let user_id: i64 = row.try_get("user_id").expect("user_id");
    let email: String = row.try_get("email").expect("email");
    let amount: f64 = row.try_get("amount").expect("amount");
    let currency: String = row.try_get("currency").expect("currency");
    let status: String = row.try_get("status").expect("status");
    let description: String = row.try_get("description").expect("description");
    let quantity: i32 = row.try_get("quantity").expect("quantity");
    let is_active: bool = row.try_get("is_active").expect("is_active");
    let metadata: String = row.try_get("metadata").expect("metadata");

    assert_eq!(id, 42);
    assert_eq!(user_id, 9001);
    assert_eq!(email, "buyer@example.com");
    assert!((amount - 12.5).abs() < 1e-9);
    assert_eq!(currency, "USD");
    assert_eq!(status, "paid");
    assert_eq!(description, "first order");
    assert_eq!(quantity, 3);
    assert!(is_active);
    assert_eq!(metadata, "{\"channel\":\"web\"}");

    h.drop_table(table).await;
}
