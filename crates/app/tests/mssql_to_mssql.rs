//! Same-vendor: MS SQL source → MS SQL sink, MS SQL storage.
//!
//! Two e2e cases:
//!   * `mssql_to_mssql_wildcard_round_trip` — `mapping = ["*"]` round-trip.
//!   * `mssql_to_mssql_incremental` — cursor pagination across batches.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mssql::mssql_pool;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mssql_to_mssql_wildcard_round_trip() {
    let ms = mssql_pool().await;

    let src_table = format!("[{db}].dbo.people_src", db = ms.database);
    let dst_table = format!("[{db}].dbo.people_dst", db = ms.database);

    let mut conn = ms.pool.get().await.unwrap();
    // The column set deliberately covers numeric types that previously
    // round-tripped as silent NULL: DECIMAL(p,s), NUMERIC(p,0)→BigInt,
    // MONEY, plus FLOAT to exercise the float bind path.
    conn.simple_query(&format!(
        "CREATE TABLE {src_table} ( \
            id        BIGINT NOT NULL, \
            name      NVARCHAR(100), \
            age       INT NOT NULL, \
            balance   DECIMAL(10,2) NULL, \
            big_id    NUMERIC(20,0) NULL, \
            ratio     FLOAT NULL, \
            payout    MONEY NULL \
        )",
    ))
    .await
    .unwrap();
    conn.simple_query(&format!(
        "CREATE TABLE {dst_table} ( \
            id        BIGINT NOT NULL, \
            name      NVARCHAR(100), \
            age       INT NOT NULL, \
            balance   DECIMAL(10,2) NULL, \
            big_id    NUMERIC(20,0) NULL, \
            ratio     FLOAT NULL, \
            payout    MONEY NULL \
        )",
    ))
    .await
    .unwrap();

    conn.simple_query(&format!(
        "INSERT INTO {src_table} (id, name, age, balance, big_id, ratio, payout) VALUES \
         (1, N'alice', 30, 123.45, 99999999999999999999, 3.14, 1000.5000), \
         (2, NULL,    41, NULL,    NULL,                 NULL, NULL), \
         (3, N'carol', 27, -0.01,  1,                    -2.5, 9999.9999)",
    ))
    .await
    .unwrap();
    drop(conn);

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "mssql"
config = {{ url = "{url}" }}

[[sinks]]
name = "snk"
type = "mssql"
config = {{ url = "{url}" }}

[[storages]]
name = "st"
type = "mssql"
config = {{ url = "{url}" }}

[flow.people]
source = "src"
sink = "snk"
storage = "st"
from = "{db}.dbo.people_src"
to = "{db}.dbo.people_dst"
batch-limit = 8

mapping = ["*"]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
        url = ms.url,
        db = ms.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    // Read back as NVARCHAR strings for numerics so we don't depend on
    // a specific tiberius FromSql impl for Decimal — string form catches
    // both silent-NULL and silent-truncation regressions.
    let mut conn = ms.pool.get().await.unwrap();
    let stream = conn
        .simple_query(&format!(
            "SELECT id, name, age, \
                    CAST(balance AS NVARCHAR(64)) AS balance_s, \
                    CAST(big_id  AS NVARCHAR(64)) AS big_id_s, \
                    CAST(ratio   AS NVARCHAR(64)) AS ratio_s, \
                    CAST(payout  AS NVARCHAR(64)) AS payout_s \
             FROM {dst_table} ORDER BY id"
        ))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    assert_eq!(rows.len(), 3, "all source rows must reach the sink");

    let row0_id: i64 = rows[0].try_get::<i64, _>(0).unwrap().unwrap();
    let row0_name: &str = rows[0].try_get::<&str, _>(1).unwrap().unwrap();
    let row0_age: i32 = rows[0].try_get::<i32, _>(2).unwrap().unwrap();
    let row0_balance: &str = rows[0].try_get::<&str, _>(3).unwrap().unwrap();
    let row0_big: &str = rows[0].try_get::<&str, _>(4).unwrap().unwrap();
    let row0_payout: &str = rows[0].try_get::<&str, _>(6).unwrap().unwrap();
    assert_eq!((row0_id, row0_name, row0_age), (1, "alice", 30));
    assert_eq!(row0_balance, "123.45", "Decimal must round-trip exactly");
    assert_eq!(
        row0_big, "99999999999999999999",
        "NUMERIC(20,0) must round-trip without truncation"
    );
    assert_eq!(row0_payout, "1000.5000", "MONEY must round-trip exactly");

    let row1_id: i64 = rows[1].try_get::<i64, _>(0).unwrap().unwrap();
    let row1_name: Option<&str> = rows[1].try_get::<&str, _>(1).unwrap();
    let row1_age: i32 = rows[1].try_get::<i32, _>(2).unwrap().unwrap();
    let row1_balance: Option<&str> = rows[1].try_get::<&str, _>(3).unwrap();
    let row1_big: Option<&str> = rows[1].try_get::<&str, _>(4).unwrap();
    let row1_ratio: Option<&str> = rows[1].try_get::<&str, _>(5).unwrap();
    let row1_payout: Option<&str> = rows[1].try_get::<&str, _>(6).unwrap();
    assert_eq!(row1_id, 2);
    assert_eq!(row1_name, None, "NULL name must round-trip as NULL");
    assert_eq!(row1_age, 41);
    assert_eq!(row1_balance, None, "NULL Decimal must round-trip as NULL");
    assert_eq!(row1_big, None, "NULL BigInt must round-trip as NULL");
    assert_eq!(row1_ratio, None, "NULL Float must round-trip as NULL");
    assert_eq!(row1_payout, None, "NULL Money must round-trip as NULL");

    let row2_id: i64 = rows[2].try_get::<i64, _>(0).unwrap().unwrap();
    let row2_name: &str = rows[2].try_get::<&str, _>(1).unwrap().unwrap();
    let row2_age: i32 = rows[2].try_get::<i32, _>(2).unwrap().unwrap();
    let row2_balance: &str = rows[2].try_get::<&str, _>(3).unwrap().unwrap();
    let row2_big: &str = rows[2].try_get::<&str, _>(4).unwrap().unwrap();
    assert_eq!((row2_id, row2_name, row2_age), (3, "carol", 27));
    assert_eq!(row2_balance, "-0.01", "negative Decimal must round-trip");
    assert_eq!(row2_big, "1");
    drop(conn);
    drop(ms);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mssql_to_mssql_incremental() {
    let ms = mssql_pool().await;

    let src_table = format!("[{db}].dbo.items_src", db = ms.database);
    let dst_table = format!("[{db}].dbo.items_dst", db = ms.database);

    let mut conn = ms.pool.get().await.unwrap();
    conn.simple_query(&format!(
        "CREATE TABLE {src_table} (id INT NOT NULL, val NVARCHAR(50) NOT NULL)",
    ))
    .await
    .unwrap();
    conn.simple_query(&format!(
        "CREATE TABLE {dst_table} (id INT NOT NULL, val NVARCHAR(50) NOT NULL)",
    ))
    .await
    .unwrap();

    let mut values = Vec::new();
    for i in 1..=15 {
        values.push(format!("({i}, N'item_{i}')"));
    }
    conn.simple_query(&format!(
        "INSERT INTO {src_table} (id, val) VALUES {}",
        values.join(", ")
    ))
    .await
    .unwrap();
    drop(conn);

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "mssql"
config = {{ url = "{url}" }}

[[sinks]]
name = "snk"
type = "mssql"
config = {{ url = "{url}" }}

[[storages]]
name = "st"
type = "mssql"
config = {{ url = "{url}" }}

[flow.items]
source = "src"
sink = "snk"
storage = "st"
from = "{db}.dbo.items_src"
to = "{db}.dbo.items_dst"
batch-limit = 10

mapping = ["*"]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
        url = ms.url,
        db = ms.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");

    app.run_once().await.expect("run_once tick 1");

    let mut conn = ms.pool.get().await.unwrap();
    let stream = conn
        .simple_query(&format!("SELECT COUNT(*) AS cnt FROM {dst_table}"))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    let count: i32 = rows[0].try_get::<i32, _>(0).unwrap().unwrap();
    assert_eq!(count, 10, "first tick must write 10 rows");

    app.run_once().await.expect("run_once tick 2");

    let stream = conn
        .simple_query(&format!("SELECT COUNT(*) AS cnt FROM {dst_table}"))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    let count: i32 = rows[0].try_get::<i32, _>(0).unwrap().unwrap();
    assert_eq!(count, 15, "second tick must accumulate 15 rows");
    drop(conn);

    app.run_once().await.expect("run_once tick 3");

    let mut conn = ms.pool.get().await.unwrap();
    let stream = conn
        .simple_query(&format!("SELECT COUNT(*) AS cnt FROM {dst_table}"))
        .await
        .unwrap();
    let rows = stream.into_first_result().await.unwrap();
    let count: i32 = rows[0].try_get::<i32, _>(0).unwrap().unwrap();
    assert_eq!(count, 15, "third tick must not duplicate rows");
    drop(conn);
    drop(ms);
}
