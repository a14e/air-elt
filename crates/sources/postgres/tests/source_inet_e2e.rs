//! PG `inet` source-side round-trip — schema introspection maps the
//! column to `DataType::Custom(PgInetType)` and the codec materialises
//! every cell as `Value::Custom(PgInetValue(IpNetwork))`, preserving
//! the netmask losslessly.

#![allow(clippy::unwrap_used)]

use air_elt_commons_pg::types::{PgInetType, PgInetValue};
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use sqlx::Executor;

#[tokio::test]
async fn inet_round_trip_preserves_mask() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE ip_rows (
                id BIGSERIAL PRIMARY KEY,
                addr INET NOT NULL
            )",
        )
        .await
        .expect("create");
    // Host /32, host /128, IPv6 host, and a /24 subnet — the codec
    // must round-trip each shape losslessly through PgInetValue.
    handle
        .pool
        .execute(
            "INSERT INTO ip_rows(addr) VALUES \
                 ('192.0.2.1'::inet), \
                 ('2001:db8::1'::inet), \
                 ('192.0.2.0/24'::inet), \
                 ('::ffff:203.0.113.42'::inet)",
        )
        .await
        .expect("seed");

    let source = PgSource::connect(
        "test_inet".to_string(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            ..Default::default()
        },
    )
    .await
    .expect("connect");

    let spec = ReadSpec {
        columns: vec!["id".into(), "addr".into()],
        table: format!("{}.ip_rows", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    source
        .validate_access(&spec)
        .await
        .expect("validate_access");
    let ctx = source.build_context(&spec).await.expect("build_context");

    // Confirm the schema introspector maps `inet` to the
    // PgInet custom descriptor.
    let schema = ctx.as_schema_provider().unwrap().schema();
    let addr_field = schema
        .fields()
        .iter()
        .find(|f| f.name == "addr")
        .expect("addr field");
    match &addr_field.data_type {
        DataType::Custom(t) => assert_eq!(t.kind(), PgInetType::KIND),
        other => panic!("expected DataType::Custom(PgInet), got {other:?}"),
    }

    let batch = source.read_batch(&spec, &ctx, None).await.expect("read");
    assert_eq!(batch.rows.len(), 4);

    // Each cell must be Value::Custom(PgInetValue(...)) with both
    // the prefix AND the host bits preserved verbatim.
    let texts: Vec<String> = batch
        .rows
        .iter()
        .map(|row| match &row.values[1] {
            Value::Custom(c) => {
                let inet = c
                    .as_any()
                    .downcast_ref::<PgInetValue>()
                    .expect("PgInetValue");
                inet.0.to_string()
            }
            other => panic!("expected Custom(PgInetValue), got {other:?}"),
        })
        .collect();
    // Both prefix and address must round-trip verbatim. PG normalises
    // `::ffff:203.0.113.42` to the embedded-IPv4 form on the wire.
    assert_eq!(
        texts,
        vec![
            "192.0.2.1/32".to_string(),
            "2001:db8::1/128".to_string(),
            "192.0.2.0/24".to_string(),
            "::ffff:203.0.113.42/128".to_string(),
        ]
    );

    handle.pool.close().await;
}
