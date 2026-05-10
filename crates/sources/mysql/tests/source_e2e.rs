#![allow(clippy::unwrap_used)]
use air_elt_commons_testing::mysql::mysql_pool;
use air_elt_core::model::ReadSpec;
use air_elt_core::traits::Source;
use air_elt_core::types::{DataType, Value};
use air_elt_source_mysql::{MySqlSource, MySqlSourceConfig};
use sqlx::Executor;

async fn seed_users(pool: &sqlx::MySqlPool) {
    pool.execute(
        "CREATE TABLE users (
            id BIGINT NOT NULL AUTO_INCREMENT PRIMARY KEY,
            name VARCHAR(64) NOT NULL,
            created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
        ) ENGINE=InnoDB",
    )
    .await
    .expect("create users");

    for i in 0..5 {
        sqlx::query("INSERT INTO users (name) VALUES (?)")
            .bind(format!("user-{i}"))
            .execute(pool)
            .await
            .expect("insert");
    }
}

#[tokio::test]
async fn describe_and_read_with_cursor() {
    let handle = mysql_pool().await;
    seed_users(&handle.pool).await;

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
        columns: vec!["id".into(), "name".into()],
        table: format!("{}.users", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 3,
        source_options: toml::Table::new(),
        needs_body: false,
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
    assert_eq!(
        schema.find("name").unwrap().data_type,
        DataType::Text { size: Some(64) }
    );

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
        .expect("read_batch tail");
    assert!(empty.rows.is_empty());
    handle.pool.close().await;
}

/// Nullable cursor with mixed NULL/non-null data. MySQL's default ASC
/// ordering puts NULLs first ("NULL is minimum"), matching the project's
/// algebra without explicit `NULLS FIRST` syntax.
#[tokio::test]
async fn read_with_nullable_cursor() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE ranked (
                id BIGINT NOT NULL PRIMARY KEY,
                `rank` INT
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create ranked");

    for (id, rank) in [(1i64, Some(1i32)), (2, None), (3, Some(3)), (4, None)] {
        sqlx::query("INSERT INTO ranked (id, `rank`) VALUES (?, ?)")
            .bind(id)
            .bind(rank)
            .execute(&handle.pool)
            .await
            .expect("insert");
    }

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
        columns: vec!["id".into(), "rank".into()],
        table: format!("{}.ranked", handle.schema),
        cursor_fields: vec!["rank".into(), "id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 2,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let ctx = source.build_context(&spec).await.expect("build_context");

    // ASC + NULL-as-min: (NULL,2), (NULL,4) come first.
    let batch = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("batch 1");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Null);
    assert_eq!(batch.rows[0].values[0], Value::Int64(2));
    assert_eq!(batch.rows[1].values[1], Value::Null);
    assert_eq!(batch.rows[1].values[0], Value::Int64(4));
    let cursor1 = batch.next_cursor.expect("cursor 1");
    assert!(
        cursor1
            .fields
            .iter()
            .any(|f| f.name == "rank" && f.value == Value::Null),
        "cursor must carry NULL rank"
    );

    // Null-aware path: ASC + NULL cursor → col > NULL becomes IS NOT NULL.
    let batch = source
        .read_batch(&spec, ctx, Some(&cursor1))
        .await
        .expect("batch 2");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Int32(1));
    assert_eq!(batch.rows[1].values[1], Value::Int32(3));
    handle.pool.close().await;
}

/// DESC + nullable cursor: MySQL's default `ORDER BY col DESC` puts NULLs
/// last. The runner forces the null-aware predicate path on DESC even when
/// the cursor value isn't NULL yet (`needs_null_aware = true` in the SQL
/// builder when the column is nullable).
#[tokio::test]
async fn read_with_nullable_cursor_desc() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE ranked_desc (
                id BIGINT NOT NULL PRIMARY KEY,
                `rank` INT
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create ranked_desc");

    for (id, rank) in [(1i64, Some(1i32)), (2, None), (3, Some(3)), (4, None)] {
        sqlx::query("INSERT INTO ranked_desc (id, `rank`) VALUES (?, ?)")
            .bind(id)
            .bind(rank)
            .execute(&handle.pool)
            .await
            .expect("insert");
    }

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
        columns: vec!["id".into(), "rank".into()],
        table: format!("{}.ranked_desc", handle.schema),
        cursor_fields: vec!["rank".into(), "id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Desc,
        limit: 2,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let ctx = source.build_context(&spec).await.expect("build_context");

    // DESC + NULL-as-min: non-null comes first, NULL last.
    let batch = source
        .read_batch(&spec, ctx.clone(), None)
        .await
        .expect("batch 1");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Int32(3));
    assert_eq!(batch.rows[1].values[1], Value::Int32(1));
    let cursor1 = batch.next_cursor.expect("cursor 1");

    // Continue: tail is NULLs in some order.
    let batch = source
        .read_batch(&spec, ctx, Some(&cursor1))
        .await
        .expect("batch 2");
    assert_eq!(batch.rows.len(), 2);
    assert_eq!(batch.rows[0].values[1], Value::Null);
    assert_eq!(batch.rows[1].values[1], Value::Null);
    handle.pool.close().await;
}

/// `tinyint(1)` is the canonical MySQL "boolean" — must surface as
/// `DataType::Bool` and round-trip through `Value::Bool`.
#[tokio::test]
async fn tinyint_one_round_trips_as_bool() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE flags (
                id BIGINT NOT NULL PRIMARY KEY,
                active TINYINT(1) NOT NULL,
                approved TINYINT(1)
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create flags");

    for (id, active, approved) in [(1i64, 1i8, Some(1i8)), (2, 0, Some(0)), (3, 1, None)] {
        sqlx::query("INSERT INTO flags (id, active, approved) VALUES (?, ?, ?)")
            .bind(id)
            .bind(active)
            .bind(approved)
            .execute(&handle.pool)
            .await
            .expect("insert");
    }

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
        columns: vec!["id".into(), "active".into(), "approved".into()],
        table: format!("{}.flags", handle.schema),
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };

    let schema = source
        .describe_schema(&spec.table)
        .await
        .expect("describe_schema");
    assert_eq!(schema.find("active").unwrap().data_type, DataType::Bool);
    assert!(!schema.find("active").unwrap().nullable);
    assert_eq!(schema.find("approved").unwrap().data_type, DataType::Bool);
    assert!(schema.find("approved").unwrap().nullable);

    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, ctx, None)
        .await
        .expect("read_batch");
    assert_eq!(batch.rows.len(), 3);
    assert_eq!(batch.rows[0].values[1], Value::Bool(true));
    assert_eq!(batch.rows[0].values[2], Value::Bool(true));
    assert_eq!(batch.rows[1].values[1], Value::Bool(false));
    assert_eq!(batch.rows[1].values[2], Value::Bool(false));
    assert_eq!(batch.rows[2].values[1], Value::Bool(true));
    assert_eq!(batch.rows[2].values[2], Value::Null);
    handle.pool.close().await;
}

/// `binary(N)` / `varbinary(N)` must surface as `DataType::Bytes { size: Some(N) }`.
#[tokio::test]
async fn binary_columns_carry_size() {
    let handle = mysql_pool().await;
    handle
        .pool
        .execute(
            "CREATE TABLE blobs (
                id BIGINT NOT NULL PRIMARY KEY,
                fixed16 BINARY(16) NOT NULL,
                varbin VARBINARY(32)
            ) ENGINE=InnoDB",
        )
        .await
        .expect("create blobs");

    let payload = vec![0xABu8; 16];
    sqlx::query("INSERT INTO blobs (id, fixed16, varbin) VALUES (?, ?, ?)")
        .bind(1i64)
        .bind(&payload)
        .bind(Some(vec![0x01u8, 0x02, 0x03]))
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

    let table = format!("{}.blobs", handle.schema);
    let schema = source.describe_schema(&table).await.expect("describe");
    assert_eq!(
        schema.find("fixed16").unwrap().data_type,
        DataType::Bytes { size: Some(16) }
    );
    assert_eq!(
        schema.find("varbin").unwrap().data_type,
        DataType::Bytes { size: Some(32) }
    );

    let spec = ReadSpec {
        columns: vec!["id".into(), "fixed16".into(), "varbin".into()],
        table,
        cursor_fields: vec!["id".into()],
        cursor_order: air_elt_core::config::model::CursorOrder::Asc,
        limit: 10,
        source_options: toml::Table::new(),
        needs_body: false,
    };
    let ctx = source.build_context(&spec).await.expect("build_context");
    let batch = source
        .read_batch(&spec, ctx, None)
        .await
        .expect("read_batch");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.rows[0].values[1], Value::Bytes(payload));
    assert_eq!(
        batch.rows[0].values[2],
        Value::Bytes(vec![0x01, 0x02, 0x03])
    );
    handle.pool.close().await;
}
