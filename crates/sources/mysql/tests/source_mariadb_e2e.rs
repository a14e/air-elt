//! Source e2e against **MariaDB**, exercising the divergence MariaDB 10.7+
//! introduces over stock MySQL: a native `UUID` column type. Stock MySQL has
//! no UUID type; this test guarantees the source's schema introspection +
//! decoder round-trip works end-to-end against a real MariaDB instance.
#![allow(clippy::unwrap_used)]

use air_elt_commons_testing::mariadb::mariadb_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_mysql::{MySqlSource, MySqlSourceConfig};
use sqlx::Executor;
use uuid::Uuid;

#[tokio::test]
async fn reads_native_uuid_column_from_mariadb() {
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

    // MariaDB validates the version/variant nibbles strictly, so the test
    // value must form a well-formed RFC 4122 UUID (version nibble 1–8,
    // variant nibble 8–b). Picking version 4 / variant 8.
    let known = Uuid::from_u128(0xdead_beef_cafe_4f00_8123_4567_89ab_cdef);
    // Bind as canonical text — MariaDB UUID columns parse text reliably
    // but apply a byte-shuffle to binary input that doesn't round-trip.
    sqlx::query("INSERT INTO accounts (id, ext) VALUES (1, ?)")
        .bind(known.to_string())
        .execute(&handle.pool)
        .await
        .expect("insert");

    let source = MySqlSource::connect(
        "test_source".to_string(),
        MySqlSourceConfig {
            url: handle.url_with_database(),
            ..Default::default()
        },
    )
    .await
    .expect("connect source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "ext".into()],
        table: format!("{}.accounts", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
    };

    let schema = source.describe_schema(&spec.table).await.expect("describe");
    assert_eq!(
        schema.find("ext").unwrap().data_type,
        DataType::Uuid,
        "MariaDB 10.7+ surfaces UUID as a first-class type — must not fall back to text/bytes"
    );

    let ctx = source.build_context(&spec).await.expect("ctx");
    let batch = source
        .read_batch(&spec, ctx, None)
        .await
        .expect("read_batch");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].values[1], Value::Uuid(known));
    handle.pool.close().await;
}
