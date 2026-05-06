//! XML allow-list behaviour split between dialects.
//!
//! The canonical proof that CockroachDB rejects an `Xml` column up front is
//! the `reject_excluded_types` unit test inside `pg_source.rs`. We cannot
//! reproduce the full e2e shape on a real Cockroach instance because Cockroach
//! has no `XML` type — `CREATE TABLE … XML` fails before our source ever sees
//! the column. The e2e test below covers the symmetric Postgres path: an XML
//! column on PG passes `validate_access` (the dialect declines to exclude any
//! type), proving the wiring doesn't accidentally reject XML on the
//! historically-supported backend.
#![allow(clippy::unwrap_used)]

use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use sqlx::Executor;

#[tokio::test]
async fn postgres_accepts_xml_column_at_validate_access() {
    let handle = pg_pool().await;
    let ddl = format!(
        "CREATE TABLE {}.docs (
            id INT PRIMARY KEY,
            payload XML
         )",
        handle.schema
    );
    handle.pool.execute(ddl.as_str()).await.unwrap();

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            dialect: Dialect::Postgres,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    let spec = ReadSpec {
        columns: vec!["id".into(), "payload".into()],
        table: format!("{}.docs", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
    };

    // PG dialect: XML is fine. The helper inside validate_access must not
    // reject it.
    source
        .validate_access(&spec)
        .await
        .expect("PG dialect must allow XML columns");
}
