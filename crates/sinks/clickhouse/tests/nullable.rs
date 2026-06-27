//! `Nullable(T)` columns: verify the parser maps them to
//! `Field { nullable: true, data_type: T }` and that the encoder's
//! NULL flag byte path round-trips both `Value::Null` and a real
//! value through CH.

use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_clickhouse::{ChSink, ChSinkConfig};

#[tokio::test]
async fn nullable_columns_round_trip_null_and_value() {
    let h = clickhouse_handle().await;
    h.exec(
        "CREATE TABLE n_t ( \
            id UInt64, \
            name Nullable(String), \
            age Nullable(Int32) \
        ) ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .expect("create table");

    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    let sink = ChSink::connect(cfg).await.expect("connect");

    let spec = WriteSpec {
        table: "n_t".to_string(),
        columns: vec!["id".into(), "name".into(), "age".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    // Sanity: the parser surfaced `nullable = true` on the two columns
    // that should carry it, and `false` on `id`.
    let provider = ctx
        .as_schema_provider()
        .expect("schema provider on ChSinkCtx");
    let schema = provider.schema();
    assert!(!schema.find("id").expect("id").nullable);
    assert!(schema.find("name").expect("name").nullable);
    assert!(schema.find("age").expect("age").nullable);

    let batch = Batch {
        rows: vec![
            Row {
                values: vec![
                    Value::UInt64(1),
                    Value::Text("alice".into()),
                    Value::Int32(30),
                ],
                body: None,
                op: RowOp::Upsert,
            },
            Row {
                values: vec![Value::UInt64(2), Value::Null, Value::Null],
                body: None,
                op: RowOp::Upsert,
            },
        ],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 2);

    // SELECT both rows and assert NULL handling. CH renders NULL as
    // `\N` in TabSeparated.
    let body = h
        .exec("SELECT id, name, age FROM n_t ORDER BY id FORMAT TabSeparated")
        .await
        .expect("select");
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "two rows");
    assert_eq!(lines[0], "1\talice\t30");
    assert_eq!(lines[1], "2\t\\N\t\\N");
}
