#![allow(clippy::unwrap_used)]
use air_elt_commons_testing::pg::pg_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use sqlx::Executor;

async fn seed_users(pool: &sqlx::PgPool) {
    pool.execute(
        "CREATE TABLE users (
            id BIGSERIAL PRIMARY KEY,
            name TEXT NOT NULL,
            created_at TIMESTAMPTZ NOT NULL DEFAULT now()
        )",
    )
    .await
    .expect("create users");

    for i in 0..5 {
        sqlx::query("INSERT INTO users (name) VALUES ($1)")
            .bind(format!("user-{i}"))
            .execute(pool)
            .await
            .expect("insert");
    }
}

#[tokio::test]
async fn describe_and_read_with_cursor() {
    let handle = pg_pool().await;
    seed_users(&handle.pool).await;

    let source = PgSource::connect(PgSourceConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.users", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 3,
    };

    source
        .validate_access(&spec)
        .await
        .expect("validate_access");

    let schema = source
        .describe_schema(&spec.table)
        .await
        .expect("describe_schema");
    let id_field = schema.find("id").unwrap();
    assert_eq!(id_field.data_type, DataType::Int64);
    assert!(!id_field.nullable);
    assert_eq!(schema.find("name").unwrap().data_type, DataType::Text);

    // First tick: no cursor, should get 3 rows
    let batch = source
        .read_batch(&spec, None)
        .await
        .expect("read_batch initial");
    assert_eq!(batch.rows.len(), 3);
    assert_eq!(batch.rows[0].values[0], Value::Int64(1));
    let next = batch.next_cursor.expect("next cursor");
    assert_eq!(next.fields[0].name, "id");
    assert_eq!(next.fields[0].value, Value::Int64(3));

    // Second tick: continue from cursor, two remaining rows
    let batch = source
        .read_batch(&spec, Some(&next))
        .await
        .expect("read_batch continued");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[0], Value::Int64(4));
    assert_eq!(batch.rows[1].values[0], Value::Int64(5));

    // Drain: cursor at the end, expect empty batch, no next cursor
    let tail = batch.next_cursor.clone().unwrap();
    let empty = source
        .read_batch(&spec, Some(&tail))
        .await
        .expect("read_batch drained");
    assert!(empty.rows.is_empty());
    assert!(empty.next_cursor.is_none());
}
