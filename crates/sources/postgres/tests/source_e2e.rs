#![allow(clippy::unwrap_used)]
use air_elt_commons_pg::Dialect;
use air_elt_commons_testing::cockroach::cockroach_pool;
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

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            ..Default::default()
        },
    )
    .await
    .expect("connect source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.users", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 3,
        source_options: toml::Table::new(),
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
    assert_eq!(schema.find("name").unwrap().data_type, DataType::text());

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
    handle.pool.close().await;
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

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_search_path(),
            ..Default::default()
        },
    )
    .await
    .expect("connect source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "rank".into()],
        table: format!("{}.ranked", handle.schema),
        cursor_fields: vec!["rank".into(), "id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 2,
        source_options: toml::Table::new(),
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
    handle.pool.close().await;
}

/// Cockroach mirror of `describe_and_read_with_cursor`: smoke-tests the full
/// validate → build_context → read_batch path against CockroachDB. Cockroach
/// speaks pgwire, so the existing source must work unchanged.
#[tokio::test]
async fn cockroach_read_batch_smoke() {
    let handle = cockroach_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE users (
                id INT PRIMARY KEY,
                name STRING NOT NULL
            )",
        )
        .await
        .expect("create users");
    for i in 1..=5i64 {
        sqlx::query("INSERT INTO users (id, name) VALUES ($1, $2)")
            .bind(i)
            .bind(format!("user-{i}"))
            .execute(&handle.pool)
            .await
            .expect("insert");
    }

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_database(),
            dialect: Dialect::Cockroach,
            ..Default::default()
        },
    )
    .await
    .expect("connect cockroach source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "name".into()],
        table: "public.users".to_string(),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
    };

    source
        .validate_access(&spec)
        .await
        .expect("validate_access");
    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, ctx, None)
        .await
        .expect("read_batch initial");
    assert_eq!(batch.rows.len(), 5);
    assert_eq!(batch.rows[0].values[0], Value::Int64(1));
    assert_eq!(batch.rows[4].values[0], Value::Int64(5));
    let cursor = batch.next_cursor.expect("cursor");
    assert_eq!(cursor.fields[0].name, "id");
    assert_eq!(cursor.fields[0].value, Value::Int64(5));
    handle.pool.close().await;
}

/// Cockroach mirror of `read_with_nullable_cursor`: NULL-cursor lexicographic
/// algebra with two cursor columns (one nullable). NULLs come first in ASC
/// order, then non-null rows.
#[tokio::test]
async fn cockroach_null_cursor_lexicographic_two_keys() {
    let handle = cockroach_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE ranked (
                id   INT PRIMARY KEY,
                rank INT
            )",
        )
        .await
        .expect("create ranked");
    for (id, rank) in [(1i64, Some(1i32)), (2, None), (3, Some(3)), (4, None)] {
        sqlx::query("INSERT INTO ranked (id, rank) VALUES ($1, $2)")
            .bind(id)
            .bind(rank)
            .execute(&handle.pool)
            .await
            .expect("insert");
    }

    let source = PgSource::connect(
        "test_source".to_string(),
        PgSourceConfig {
            url: handle.url_with_database(),
            dialect: Dialect::Cockroach,
            ..Default::default()
        },
    )
    .await
    .expect("connect cockroach source");

    let spec = ReadSpec {
        columns: vec!["id".into(), "rank".into()],
        table: "public.ranked".to_string(),
        cursor_fields: vec!["rank".into(), "id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 2,
        source_options: toml::Table::new(),
    };

    let ctx = source.build_context(&spec).await.expect("build_context");

    // First batch: NULL ranks come first under ASC NULLS FIRST.
    let batch = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("batch 1");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Null);
    assert_eq!(batch.rows[1].values[1], Value::Null);
    let cursor1 = batch.next_cursor.expect("cursor after batch 1");
    assert!(
        cursor1
            .fields
            .iter()
            .any(|f| f.name == "rank" && f.value == Value::Null),
        "cursor must carry NULL rank"
    );

    // Second batch: cursor=(NULL, last_id) → null-aware path picks up
    // non-null ranks 1 and 3.
    let batch = source
        .read_batch(&spec, ctx.clone(), Some(&cursor1))
        .await
        .expect("batch 2");
    assert_eq!(batch.rows.len(), 2);
    // Cockroach normalises `INT` to `INT8` (i64) — the schema introspection
    // reads `udt_name = 'int8'`, which maps to `DataType::Int64`.
    assert_eq!(batch.rows[0].values[1], Value::Int64(1));
    assert_eq!(batch.rows[1].values[1], Value::Int64(3));

    // Drain.
    let cursor2 = batch.next_cursor.expect("cursor after batch 2");
    let empty = source
        .read_batch(&spec, ctx, Some(&cursor2))
        .await
        .expect("drain");
    assert!(empty.rows.is_empty());
    handle.pool.close().await;
}
