//! E2e round-trip tests for CH structural types: Array(T), Map(K,V),
//! Tuple(...), and Nested(...).
//!
//! Each test:
//!  1. Creates a table with the relevant CH column type.
//!  2. Inserts one row via `ChSink::write_batch` (RowBinary path).
//!  3. Reads the value back via a direct SQL query.
//!  4. Asserts that CH received the correct values.
//!
//! Array/Map/Tuple go through the Custom(Ch*Type) / Custom(Ch*Value)
//! path added for AIR-22/AIR-23.

use air_elt_commons_clickhouse::types::array::ChArrayValue;
use air_elt_commons_clickhouse::types::map::ChMapValue;
use air_elt_commons_clickhouse::types::tuple::ChTupleValue;
use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_clickhouse::{ChSink, ChSinkConfig};

#[tokio::test]
async fn round_trip_array_int32() {
    let h = clickhouse_handle().await;
    h.exec("CREATE TABLE arr_t (id UInt64, vals Array(Int32)) ENGINE = MergeTree() ORDER BY id")
        .await
        .expect("create table");

    let sink = connect_sink(&h).await;
    let spec = spec("arr_t", &["id", "vals"]);
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let elem_type = air_elt_core::types::data_type::DataType::Int32;
    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChArrayValue {
                    element_type: elem_type,
                    elements: vec![Value::Int32(10), Value::Int32(20), Value::Int32(30)],
                })),
            ],
            body: None,
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 1);

    let body = h
        .exec("SELECT toString(vals) FROM arr_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    assert_eq!(body.trim(), "[10,20,30]");
}

#[tokio::test]
async fn round_trip_empty_array() {
    let h = clickhouse_handle().await;
    h.exec(
        "CREATE TABLE empty_arr_t (id UInt64, vals Array(Int32)) ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .expect("create table");

    let sink = connect_sink(&h).await;
    let spec = spec("empty_arr_t", &["id", "vals"]);
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChArrayValue {
                    element_type: air_elt_core::types::data_type::DataType::Int32,
                    elements: vec![],
                })),
            ],
            body: None,
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    sink.write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");

    let body = h
        .exec("SELECT toString(vals) FROM empty_arr_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    assert_eq!(body.trim(), "[]");
}

#[tokio::test]
async fn round_trip_map_string_int32() {
    let h = clickhouse_handle().await;
    h.exec(
        "CREATE TABLE map_t (id UInt64, attrs Map(String, Int32)) ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .expect("create table");

    let sink = connect_sink(&h).await;
    let spec = spec("map_t", &["id", "attrs"]);
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChMapValue {
                    entries: vec![
                        (Value::Text("x".into()), Value::Int32(100)),
                        (Value::Text("y".into()), Value::Int32(200)),
                    ],
                })),
            ],
            body: None,
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 1);

    let body = h
        .exec("SELECT toString(attrs) FROM map_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    // CH renders Map as `{key:value, ...}` in TabSeparated.
    assert!(
        body.contains("x") && body.contains("100"),
        "map round-trip: {body}"
    );
    assert!(
        body.contains("y") && body.contains("200"),
        "map round-trip: {body}"
    );
}

#[tokio::test]
async fn round_trip_tuple_int32_string() {
    let h = clickhouse_handle().await;
    h.exec(
        "CREATE TABLE tuple_t (id UInt64, tup Tuple(a Int32, b String)) \
         ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .expect("create table");

    let sink = connect_sink(&h).await;
    let spec = spec("tuple_t", &["id", "tup"]);
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChTupleValue {
                    fields: vec![Value::Int32(99), Value::Text("done".into())],
                })),
            ],
            body: None,
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 1);

    let body = h
        .exec("SELECT toString(tup) FROM tuple_t WHERE id = 1 FORMAT TabSeparated")
        .await
        .expect("select");
    // CH renders Tuple as `(a,b,...)`; strings are single-quote escaped.
    assert_eq!(body.trim(), r"(99,\'done\')");
}

/// Nested(name1 Type1, name2 Type2) is CH sugar for
/// `Array(Tuple(name1 Type1, name2 Type2))`. Under the hood CH stores
/// each sub-column as a separate Array: `items.label Array(String)`,
/// `items.qty Array(Int32)`.  We reference them by dotted name in both
/// the INSERT column list and SELECT.
#[tokio::test]
async fn round_trip_nested_as_arrays() {
    let h = clickhouse_handle().await;
    h.exec(
        "CREATE TABLE nested_t (id UInt64, items Nested(label String, qty Int32)) \
         ENGINE = MergeTree() ORDER BY id",
    )
    .await
    .expect("create table");

    let sink = connect_sink(&h).await;
    let spec = WriteSpec {
        table: "nested_t".to_string(),
        columns: vec!["id".into(), "items.label".into(), "items.qty".into()],
        conflict: None,
        sink_options: toml::Table::new(),
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChArrayValue {
                    element_type: air_elt_core::types::data_type::DataType::Text { size: None },
                    elements: vec![Value::Text("a".into()), Value::Text("b".into())],
                })),
                Value::Custom(Box::new(ChArrayValue {
                    element_type: air_elt_core::types::data_type::DataType::Int32,
                    elements: vec![Value::Int32(1), Value::Int32(2)],
                })),
            ],
            body: None,
            op: RowOp::Upsert,
        }],
        next_cursor: None,
    };
    let report = sink
        .write_batch(&spec, &ctx, batch, false)
        .await
        .expect("write_batch");
    assert_eq!(report.rows_written(), 1);

    let body = h
        .exec(
            "SELECT toString(items.label), toString(items.qty) \
             FROM nested_t WHERE id = 1 FORMAT TabSeparated",
        )
        .await
        .expect("select");
    let cells: Vec<&str> = body.trim().split('\t').collect();
    assert_eq!(cells.len(), 2, "expected two nested columns: {body}");
    // CH TabSeparated escapes single quotes in array string elements.
    assert_eq!(cells[0], r"[\'a\',\'b\']");
    assert_eq!(cells[1], "[1,2]");
}

// ---- helpers -----------------------------------------------------------

fn spec(table: &str, columns: &[&str]) -> WriteSpec {
    WriteSpec {
        table: table.to_string(),
        columns: columns.iter().map(|c| c.to_string()).collect(),
        conflict: None,
        sink_options: toml::Table::new(),
    }
}

async fn connect_sink(h: &air_elt_commons_testing::clickhouse::ClickHouseTestHandle) -> ChSink {
    let cfg = ChSinkConfig {
        url: h.url.clone(),
        database: h.database.clone(),
        ..Default::default()
    };
    ChSink::connect(cfg).await.expect("connect")
}
