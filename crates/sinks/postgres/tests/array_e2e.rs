//! Native PG array round-trip e2e (AIR-124): write `Value::Array(...)`
//! through the pg sink into `int4[]` / `text[]` columns, then read the
//! rows back through the pg source and assert the arrays are identical.
//!
//! This is the first DB round-trip over the array binding path
//! (`sink_bind::push_array` / `push_typed_null_array`) paired with the
//! source decoder (`codec::decode_array`). It exercises the three shapes
//! that distinguish the array arms from the scalar ones:
//!
//! * a NULL *element* inside a non-null array (`element_nullable`),
//! * an empty array (dispatch must come from the declared column element
//!   type, not the runtime values),
//! * a whole-column NULL array (the typed-NULL array path).
#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::{Batch, ReadSpec, Row as CoreRow, WriteSpec};
use air_elt_core::traits::{Sink, Source};
use air_elt_core::types::{DataType, Value};
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use sqlx::Executor;

/// End-to-end primitive-array identity round-trip.
///
/// 1. Create a table with `int4[]` and `text[]` columns.
/// 2. Push four rows through the sink covering: plain arrays, a NULL
///    element inside an array, an empty array, and whole-column NULL
///    arrays.
/// 3. Read every row back through the pg source.
/// 4. Assert each `Value::Array` cell round-trips cell-for-cell,
///    including the NULL element, the empty array, and the NULL column.
#[tokio::test]
async fn primitive_arrays_round_trip_through_sink_and_source() {
    let handle = pg_pool().await;

    handle
        .pool
        .execute(
            format!(
                "CREATE TABLE {}.arr (
                    id INT NOT NULL PRIMARY KEY,
                    tags INT4[],
                    names TEXT[]
                )",
                handle.schema
            )
            .as_str(),
        )
        .await
        .unwrap();

    // ---- schema introspection -----------------------------------------
    // Confirm the source maps the array columns to `DataType::Array` with
    // nullable elements before we round-trip any data through them.
    let source = PgSource::connect(
        "array_test_source".into(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let read_spec = ReadSpec {
        columns: vec!["id".into(), "tags".into(), "names".into()],
        table: format!("{}.arr", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let read_ctx = source.build_context(&read_spec).await.unwrap();
    let schema = read_ctx.as_schema_provider().unwrap().schema();

    let tags_field = schema.fields().iter().find(|f| f.name == "tags").unwrap();
    assert_eq!(
        tags_field.data_type,
        DataType::Array {
            element: Some(Box::new(DataType::Int32)),
            element_nullable: true,
        },
        "int4[] must introspect to Array(Int32) with nullable elements"
    );
    let names_field = schema.fields().iter().find(|f| f.name == "names").unwrap();
    assert_eq!(
        names_field.data_type,
        DataType::Array {
            element: Some(Box::new(DataType::Text { size: None })),
            element_nullable: true,
        },
        "text[] must introspect to Array(Text) with nullable elements"
    );

    // ---- sink path -----------------------------------------------------
    let sink = PgSink::connect(PgSinkConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .unwrap();

    let spec = WriteSpec {
        columns: vec!["id".into(), "tags".into(), "names".into()],
        table: format!("{}.arr", handle.schema),
        conflict: None,
        sink_options: toml::Table::new(),
    };

    // Row 1: plain populated arrays.
    let row1 = CoreRow::upsert(vec![
        Value::Int32(1),
        Value::Array(vec![Value::Int32(10), Value::Int32(20), Value::Int32(30)]),
        Value::Array(vec![
            Value::Text("alpha".into()),
            Value::Text("beta".into()),
        ]),
    ]);
    // Row 2: a NULL element inside each array (exercises element_nullable).
    let row2 = CoreRow::upsert(vec![
        Value::Int32(2),
        Value::Array(vec![Value::Int32(1), Value::Null, Value::Int32(3)]),
        Value::Array(vec![Value::Null, Value::Text("gamma".into())]),
    ]);
    // Row 3: empty arrays (dispatch must come from the declared element type).
    let row3 = CoreRow::upsert(vec![
        Value::Int32(3),
        Value::Array(vec![]),
        Value::Array(vec![]),
    ]);
    // Row 4: whole-column NULL arrays (the typed-NULL array path).
    let row4 = CoreRow::upsert(vec![Value::Int32(4), Value::Null, Value::Null]);

    let batch = Batch {
        rows: vec![row1, row2, row3, row4],
        next_cursor: None,
    };
    let ctx = sink.build_context(&spec).await.unwrap();
    let report = sink.write_batch(&spec, &ctx, batch, false).await.unwrap();
    assert_eq!(report.rows_written(), 4);

    // ---- source path ---------------------------------------------------
    let read_batch = source
        .read_batch(&read_spec, &read_ctx, None)
        .await
        .unwrap();
    assert_eq!(read_batch.rows.len(), 4);

    // Row 1: populated arrays round-trip cell-for-cell.
    assert_eq!(
        read_batch.rows[0].values[1],
        Value::Array(vec![Value::Int32(10), Value::Int32(20), Value::Int32(30)]),
    );
    // `Value`'s PartialEq is cross-numeric (Int32(10) == Int64(10)), so the
    // assertion above does not by itself pin the decoded element variant.
    // Pin it explicitly: an `int4[]` column must decode each element as
    // `Value::Int32`, never a wider variant.
    match &read_batch.rows[0].values[1] {
        Value::Array(items) => assert!(
            items.iter().all(|v| matches!(v, Value::Int32(_))),
            "int4[] elements must decode as Value::Int32, got {items:?}"
        ),
        other => panic!("expected Value::Array, got {other:?}"),
    }
    assert_eq!(
        read_batch.rows[0].values[2],
        Value::Array(vec![
            Value::Text("alpha".into()),
            Value::Text("beta".into())
        ]),
    );

    // Row 2: the NULL element survives at the same position.
    assert_eq!(
        read_batch.rows[1].values[1],
        Value::Array(vec![Value::Int32(1), Value::Null, Value::Int32(3)]),
    );
    assert_eq!(
        read_batch.rows[1].values[2],
        Value::Array(vec![Value::Null, Value::Text("gamma".into())]),
    );

    // Row 3: empty arrays come back as empty arrays (not NULL).
    assert_eq!(read_batch.rows[2].values[1], Value::Array(vec![]));
    assert_eq!(read_batch.rows[2].values[2], Value::Array(vec![]));

    // Row 4: NULL columns stay NULL (distinct from an empty array).
    assert_eq!(read_batch.rows[3].values[1], Value::Null);
    assert_eq!(read_batch.rows[3].values[2], Value::Null);

    handle.pool.close().await;
}
