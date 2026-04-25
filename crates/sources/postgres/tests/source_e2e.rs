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

    let ctx = source.build_context(&spec).await.expect("build_context");

    let batch = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("read_batch initial");
    assert_eq!(batch.rows.len(), 3);
    assert_eq!(batch.rows[0].values[0], Value::Int64(1));
    let next = batch.next_cursor.expect("next cursor");
    assert_eq!(next.fields[0].name, "id");
    assert_eq!(next.fields[0].value, Value::Int64(3));

    let batch = source
        .read_batch(&spec, ctx.clone(), Some(&next))
        .await
        .expect("read_batch continued");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[0], Value::Int64(4));
    assert_eq!(batch.rows[1].values[0], Value::Int64(5));

    let tail = batch.next_cursor.clone().unwrap();
    let empty = source
        .read_batch(&spec, ctx, Some(&tail))
        .await
        .expect("read_batch drained");
    assert!(empty.rows.is_empty());
    assert!(empty.next_cursor.is_none());
}

/// Nullable cursor with mixed NULL/non-null data. With `NULL < everything`
/// algebra and `ASC NULLS FIRST`, NULLs are read first, then non-null values.
#[tokio::test]
async fn read_with_nullable_cursor() {
    let handle = pg_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE ranked (
                id BIGSERIAL PRIMARY KEY,
                rank INT
            )",
        )
        .await
        .expect("create ranked");

    // rank: 1, NULL, 3, NULL
    // ASC NULLS FIRST ordering by (rank, id): (NULL,2), (NULL,4), (1,1), (3,3)
    for (id, rank) in [(1, Some(1)), (2, None), (3, Some(3)), (4, None)] {
        sqlx::query("INSERT INTO ranked (id, rank) VALUES ($1, $2)")
            .bind(id as i64)
            .bind(rank)
            .execute(&handle.pool)
            .await
            .expect("insert");
    }

    let source = PgSource::connect(PgSourceConfig {
        url: handle.url_with_search_path(),
        ..Default::default()
    })
    .await
    .expect("connect source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "rank".into()],
        table: format!("{}.ranked", handle.schema),
        cursor_fields: vec!["rank".into(), "id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 2,
    };

    let ctx = source.build_context(&spec).await.expect("build_context");

    // First batch: NULL rows come first (NULLS FIRST): (NULL,2), (NULL,4)
    let batch = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("batch 1");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Null);
    assert_eq!(batch.rows[0].values[0], Value::Int64(2));
    assert_eq!(batch.rows[1].values[1], Value::Null);
    assert_eq!(batch.rows[1].values[0], Value::Int64(4));
    let cursor1 = batch.next_cursor.expect("cursor after batch 1");
    assert!(
        cursor1
            .fields
            .iter()
            .any(|f| f.name == "rank" && f.value == Value::Null),
        "cursor must carry NULL rank"
    );

    // Second batch: cursor=(NULL,4) → null-aware path.
    // ASC + NULL cursor: col > NULL → IS NOT NULL. Gets non-null rows: (1,1), (3,3)
    let batch = source
        .read_batch(&spec, ctx.clone(), Some(&cursor1))
        .await
        .expect("batch 2");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Int32(1));
    assert_eq!(batch.rows[1].values[1], Value::Int32(3));

    // Drain
    let cursor2 = batch.next_cursor.expect("cursor after batch 2");
    let empty = source
        .read_batch(&spec, ctx, Some(&cursor2))
        .await
        .expect("drain");
    assert!(empty.rows.is_empty());
}
