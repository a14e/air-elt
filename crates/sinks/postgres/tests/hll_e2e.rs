//! HLL round-trip e2e: write `Value::Custom(PgHllValue(...))` through the
//! pg sink, read back through the pg source, byte-for-byte equality.
#![allow(clippy::unwrap_used)]

use air_elt_commons_pg::types::{PgHllType, PgHllValue};
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::{Batch, ReadSpec, Row as CoreRow, WriteSpec};
use air_elt_core::traits::{Sink, Source};
use air_elt_core::types::{DataType, DynType, Value};
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use sqlx::Executor;

/// End-to-end HLL identity round-trip:
///
/// 1. Create a table with an `hll` column (the test handle pre-installed
///    the extension at container init).
/// 2. Push two rows through the sink: one carrying a server-built HLL
///    sketch (`hll_add_agg`) re-marshalled into `Value::Custom`, and one
///    carrying NULL.
/// 3. Read both back through the pg source.
/// 4. Assert the bytes are identical and that the HLL is decoded as a
///    `Value::Custom(PgHllValue)` — confirming the codec wraps and
///    unwraps consistently.
#[tokio::test]
async fn hll_round_trip_through_sink_and_source() {
    let handle = pg_pool().await;

    // Build an HLL sketch on the server, pull its bytes back so we
    // have a realistic on-wire payload to feed through the sink. This
    // exercises the actual hll extension's binary representation
    // rather than a hand-rolled bytestring.
    handle
        .pool
        .execute(
            format!(
                "CREATE TABLE {}.t (id INT NOT NULL PRIMARY KEY, sketch hll)",
                handle.schema
            )
            .as_str(),
        )
        .await
        .unwrap();

    // `hll_send` is the HLL extension's binary output function — it
    // returns `bytea`, which is the wire format we'll feed back into
    // the sink. A direct `::bytea` cast is *not* defined by the
    // extension, so we go through the explicit send function.
    let (sketch_bytes,): (Vec<u8>,) =
        sqlx::query_as("SELECT hll_send(hll_add(hll_empty(), hll_hash_text('alpha')))")
            .fetch_one(&handle.pool)
            .await
            .unwrap();
    assert!(
        !sketch_bytes.is_empty(),
        "server-built HLL sketch must be non-empty"
    );

    // ---- sink path -----------------------------------------------------
    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .unwrap();

    let spec = WriteSpec {
        columns: vec!["id".into(), "sketch".into()],
        table: format!("{}.t", handle.schema),
        conflict: None,
    };

    let schema = sink.describe_schema(&spec.table).await.unwrap();
    let sketch_field = schema.find("sketch").unwrap();
    match &sketch_field.data_type {
        DataType::Custom(t) => assert_eq!(t.kind(), "postgresql.hll"),
        other => panic!("expected DataType::Custom(hll), got {other:?}"),
    }
    assert!(
        sketch_field.nullable,
        "no NOT NULL on column → must be reported nullable"
    );

    let batch = Batch {
        rows: vec![
            CoreRow::upsert(vec![
                Value::Int32(1),
                Value::Custom(Box::new(PgHllValue(sketch_bytes.clone()))),
            ]),
            // NULL through the typed-NULL path — exercises the
            // `::hll` cast on null binds.
            CoreRow::upsert(vec![Value::Int32(2), Value::Null]),
        ],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.unwrap();
    let report = sink.write_batch(&spec, &ctx, batch, false).await.unwrap();
    assert_eq!(report.rows_written, 2);

    // ---- source path ---------------------------------------------------
    let source = PgSource::connect(
        "hll_test_source".into(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let read_spec = ReadSpec {
        columns: vec!["id".into(), "sketch".into()],
        table: format!("{}.t", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let read_ctx = source.build_context(&read_spec).await.unwrap();
    let read_batch = source
        .read_batch(&read_spec, &read_ctx, None)
        .await
        .unwrap();
    assert_eq!(read_batch.rows.len(), 2);

    // Row 1: the HLL sketch round-trips byte-exact.
    match &read_batch.rows[0].values[1] {
        Value::Custom(v) => {
            let hll = v
                .as_any()
                .downcast_ref::<PgHllValue>()
                .expect("value must be PgHllValue");
            assert_eq!(
                hll.0, sketch_bytes,
                "HLL bytes must round-trip byte-for-byte"
            );
            assert_eq!(
                {
                    let dt = v.dyn_type();
                    dt.kind().to_string()
                },
                PgHllType.kind()
            );
        }
        other => panic!("expected Value::Custom(PgHllValue), got {other:?}"),
    }

    // Row 2: NULL stays NULL.
    assert_eq!(read_batch.rows[1].values[1], Value::Null);

    // Server-side sanity: cardinality of the round-tripped sketch is 1
    // (we hashed exactly one element). Confirms the bytes we wrote are
    // a *valid* HLL — not just bit-equal nonsense.
    let (cardinality,): (f64,) = sqlx::query_as(&format!(
        "SELECT hll_cardinality(sketch) FROM {}.t WHERE id = 1",
        handle.schema
    ))
    .fetch_one(&handle.pool)
    .await
    .unwrap();
    assert!(
        (cardinality - 1.0).abs() < 1e-6,
        "round-tripped HLL must report cardinality 1, got {cardinality}"
    );

    handle.pool.close().await;
}
