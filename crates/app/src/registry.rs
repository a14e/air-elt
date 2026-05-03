//! Wire connector factories into a fresh `Registry`.

use std::sync::Arc;

use air_elt_core::registry::Registry;
use air_elt_sink_mongodb::MongoSinkFactory;
use air_elt_sink_mysql::MySqlSinkFactory;
use air_elt_sink_postgres::PgSinkFactory;
use air_elt_source_mongodb::MongoSourceFactory;
use air_elt_source_mysql::MySqlSourceFactory;
use air_elt_source_postgres::PgSourceFactory;
use air_elt_storage_mongodb::MongoStorageFactory;
use air_elt_storage_mysql::MySqlStorageFactory;
use air_elt_storage_postgres::PgStorageFactory;

pub fn build_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_source("postgres", Arc::new(PgSourceFactory));
    registry.register_sink("postgres", Arc::new(PgSinkFactory));
    registry.register_storage("postgres", Arc::new(PgStorageFactory));
    registry.register_source("mysql", Arc::new(MySqlSourceFactory));
    registry.register_sink("mysql", Arc::new(MySqlSinkFactory));
    registry.register_storage("mysql", Arc::new(MySqlStorageFactory));
    registry.register_source("mongodb", Arc::new(MongoSourceFactory));
    registry.register_sink("mongodb", Arc::new(MongoSinkFactory));
    registry.register_storage("mongodb", Arc::new(MongoStorageFactory));
    registry
}
