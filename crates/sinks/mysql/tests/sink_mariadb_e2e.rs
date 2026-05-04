//! Sink e2e against **MariaDB**. Mirror of the source-side UUID test: the
//! sink must bind a `Value::Uuid` against a native `UUID` column on MariaDB
//! 10.7+ and round-trip the value byte-for-byte.
#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mariadb::mariadb_pool;
use air_elt_core::model::{Batch, Row as CoreRow, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::{DataType, Value};
use air_elt_sink_mysql::{MySqlSink, MySqlSinkConfig};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn writes_native_uuid_column_to_mariadb() {
    let handle = mariadb_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE accounts (
                id BIGINT NOT NULL PRIMARY KEY,
                ext UUID NOT NULL
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create accounts");

    let sink = MySqlSink::connect(MySqlSinkConfig {
        url: handle.url_with_database(),
        ..Default::default()
    })
    .await
    .expect("connect sink");

    let spec = WriteSpec {
        columns: vec!["id".into(), "ext".into()],
        table: format!("{}.accounts", handle.schema),
        conflict: None,
    };

    let schema = sink.describe_schema(&spec.table).await.expect("describe");
    assert_eq!(schema.find("ext").unwrap().data_type, DataType::Uuid);

    let known = Uuid::from_u128(0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    let batch = Batch {
        rows: vec![CoreRow::upsert(vec![Value::Int64(1), Value::Uuid(known)])],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.expect("ctx");
    let report = sink.write_batch(&spec, ctx, &batch).await.expect("write");
    assert_eq!(report.rows_written, 1);

    // Read back through the source decoder via raw bytes (MariaDB returns
    // UUID columns as 36-char canonical text, see source codec) and parse
    // with `Uuid::parse_str` — direct `query_as::<Uuid>` would fail because
    // sqlx-mysql expects 16 binary bytes for `Uuid`.
    let (id, ext): (i64, Vec<u8>) = sqlx::query_as("SELECT id, ext FROM accounts WHERE id = 1")
        .fetch_one(&handle.pool)
        .await
        .expect("select");
    assert_eq!(id, 1);
    let text = std::str::from_utf8(&ext).expect("uuid text");
    let got = Uuid::parse_str(text).expect("parse");
    assert_eq!(got, known);
}
