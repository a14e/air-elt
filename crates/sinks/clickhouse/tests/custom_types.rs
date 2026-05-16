//! End-to-end round-trip for every CH custom type the sink supports:
//! IPv4, IPv6, FixedString(N), Enum8, Enum16, and a
//! `SimpleAggregateFunction(sum, UInt64)` state (the simple aggregate
//! is used because its RowBinary state is just the U64 value — picking
//! TDigest/DDSketch here would require ferrying a precomputed opaque
//! state through the test, which has no test value beyond what the
//! simple-aggregate case already exercises).
//!
//! Each row is built by the sink via `write_batch`, then read back via
//! a direct SQL query through the test handle. We verify the value
//! survives the full RowBinary encode → CH parse → SQL projection
//! pipeline.

use std::net::{Ipv4Addr, Ipv6Addr};

use air_elt_commons_clickhouse::types::aggregate_state::ChAggregateStateValue;
use air_elt_commons_clickhouse::types::enums::ChEnumValue;
use air_elt_commons_clickhouse::types::fixed_string::ChFixedStringValue;
use air_elt_commons_clickhouse::types::ip::{ChIpv4Value, ChIpv6Value};
use air_elt_commons_testing::clickhouse::clickhouse_handle;
use air_elt_core::model::{Batch, Row, RowOp, WriteSpec};
use air_elt_core::traits::Sink;
use air_elt_core::types::Value;
use air_elt_sink_clickhouse::{ChSink, ChSinkConfig};

#[tokio::test]
async fn round_trip_all_custom_types() {
    let h = clickhouse_handle().await;
    // One table covering every custom-kind path through the encoder.
    // SimpleAggregateFunction(sum, UInt64) stores the U64 value as the
    // state — round-tripping an arbitrary 8-byte payload is enough to
    // exercise the opaque-bytes path that `quantilesTDigest` etc. would
    // share.
    h.exec(
        "CREATE TABLE custom_t ( \
            id UInt64, \
            v4 IPv4, \
            v6 IPv6, \
            fs FixedString(8), \
            e8 Enum8('hello' = 1, 'world' = 2), \
            e16 Enum16('alpha' = 10, 'beta' = 20), \
            agg SimpleAggregateFunction(sum, UInt64) \
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
        table: "custom_t".to_string(),
        columns: vec![
            "id".into(),
            "v4".into(),
            "v6".into(),
            "fs".into(),
            "e8".into(),
            "e16".into(),
            "agg".into(),
        ],
        conflict: None,
    };
    sink.validate_access(&spec).await.expect("validate_access");
    let ctx = sink.build_context(&spec).await.expect("build_context");

    let v4 = Ipv4Addr::new(192, 168, 0, 1);
    let v6: Ipv6Addr = "2001:db8::1".parse().expect("valid IPv6 literal");
    let fs_bytes = b"airelt-x".to_vec(); // exactly 8 bytes
    // SimpleAggregateFunction(sum, UInt64) state = the UInt64 value
    // serialised little-endian.
    let agg_value: u64 = 0x0102_0304_0506_0708;
    let agg_bytes = agg_value.to_le_bytes().to_vec();

    let batch = Batch {
        rows: vec![Row {
            values: vec![
                Value::UInt64(1),
                Value::Custom(Box::new(ChIpv4Value(v4))),
                Value::Custom(Box::new(ChIpv6Value(v6))),
                Value::Custom(Box::new(ChFixedStringValue {
                    bytes: fs_bytes.clone(),
                })),
                Value::Custom(Box::new(ChEnumValue {
                    name: "world".to_string(),
                    value: 2,
                })),
                Value::Custom(Box::new(ChEnumValue {
                    name: "alpha".to_string(),
                    value: 10,
                })),
                Value::Custom(Box::new(ChAggregateStateValue {
                    bytes: agg_bytes,
                    fn_name: "sum".to_string(),
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
    assert_eq!(report.rows_written, 1);

    // SELECT each column back. Use TabSeparated for predictable output
    // and `toString` to coerce CH's native rendering into something we
    // can string-match on.
    let body = h
        .exec(
            "SELECT toString(v4), toString(v6), toString(fs), \
                    toString(e8), toString(e16), toString(agg) \
             FROM custom_t WHERE id = 1 FORMAT TabSeparated",
        )
        .await
        .expect("select");
    let line = body.lines().next().expect("one row");
    let cells: Vec<&str> = line.split('\t').collect();
    assert_eq!(cells.len(), 6, "unexpected row shape: {line:?}");
    assert_eq!(cells[0], "192.168.0.1", "IPv4 round-trip");
    assert_eq!(cells[1], "2001:db8::1", "IPv6 round-trip");
    assert_eq!(cells[2], "airelt-x", "FixedString round-trip");
    assert_eq!(cells[3], "world", "Enum8 round-trip");
    assert_eq!(cells[4], "alpha", "Enum16 round-trip");
    // SimpleAggregateFunction(sum, UInt64) renders as the U64 value.
    assert_eq!(cells[5], agg_value.to_string(), "agg state round-trip");
}
