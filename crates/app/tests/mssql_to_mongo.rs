//! Cross-vendor: MS SQL source → MongoDB sink.

#![allow(clippy::unwrap_used)]

use air_elt_app::App;
use air_elt_commons_testing::mongo::mongo_pool;
use air_elt_commons_testing::mssql::mssql_pool;
use bson::doc;
use futures::TryStreamExt;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mssql_to_mongo_wildcard_round_trip() {
    let ms = mssql_pool().await;
    let mongo = mongo_pool().await;

    let src_table = format!("[{db}].dbo.contacts", db = ms.database);

    let mut conn = ms.pool.get().await.unwrap();
    conn.simple_query(&format!(
        "CREATE TABLE {src_table} ( \
            id   INT NOT NULL, \
            name NVARCHAR(100), \
            email NVARCHAR(200) \
        )",
    ))
    .await
    .unwrap();
    conn.simple_query(&format!(
        "INSERT INTO {src_table} (id, name, email) VALUES \
         (1, N'alice', N'alice@example.com'), \
         (2, N'bob', NULL), \
         (3, N'carol', N'carol@example.com')",
    ))
    .await
    .unwrap();
    drop(conn);

    let config_toml = format!(
        r#"
[[sources]]
name = "src"
type = "mssql"
config = {{ url = "{ms_url}" }}

[[sinks]]
name = "snk"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{mongo_db}", collection = "contacts" }}

[[storages]]
name = "st"
type = "mongodb"
config = {{ url = "{mongo_url}", database = "{mongo_db}", collection = "air_elt_state" }}

[flow.contacts]
source = "src"
sink = "snk"
storage = "st"
from = "{src_table}"
batch-limit = 8

mapping = ["*"]

cursor = {{ fields = ["id"], order = "asc", interval = "100ms" }}
"#,
        ms_url = ms.url,
        src_table = src_table,
        mongo_url = mongo.url,
        mongo_db = mongo.database,
    );

    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config.toml");
    std::fs::write(&config_path, &config_toml).unwrap();
    let app = App::from_path(&config_path).expect("App::from_path");
    app.run_once().await.expect("run_once");

    let coll = mongo
        .client
        .database(&mongo.database)
        .collection::<bson::Document>("contacts");

    let mut cursor = coll.find(doc! {}).sort(doc! {"id": 1}).await.unwrap();
    let mut docs = Vec::new();
    while let Some(d) = cursor.try_next().await.unwrap() {
        docs.push(d);
    }

    assert_eq!(docs.len(), 3, "all MS SQL rows must reach MongoDB");
    assert_eq!(docs[0].get_i32("id").unwrap(), 1);
    assert_eq!(docs[0].get_str("name").unwrap(), "alice");
    assert_eq!(docs[0].get_str("email").unwrap(), "alice@example.com");
    assert_eq!(docs[1].get_i32("id").unwrap(), 2);
    assert_eq!(docs[1].get_str("name").unwrap(), "bob");
    assert!(
        matches!(docs[1].get("email"), Some(bson::Bson::Null) | None),
        "NULL email must cross vendor boundary"
    );
    assert_eq!(docs[2].get_i32("id").unwrap(), 3);
    assert_eq!(docs[2].get_str("name").unwrap(), "carol");
    drop(ms);
    drop(mongo);
}
