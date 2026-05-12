//! Wire connector factories into a fresh `Registry`.

use std::sync::Arc;

use air_elt_core::registry::Registry;
use air_elt_sink_clickhouse::ChSinkFactory;
use air_elt_sink_mongodb::MongoSinkFactory;
use air_elt_sink_mysql::MySqlSinkFactory;
use air_elt_sink_postgres::PgSinkFactory;
use air_elt_source_mongo_cdc::MongoCdcSourceFactory;
use air_elt_source_mongodb::MongoSourceFactory;
use air_elt_source_mysql::MySqlSourceFactory;
use air_elt_source_postgres::PgSourceFactory;
use air_elt_storage_mongodb::MongoStorageFactory;
use air_elt_storage_mysql::MySqlStorageFactory;
use air_elt_storage_postgres::PgStorageFactory;

pub fn build_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_source("postgres", Arc::new(PgSourceFactory::postgres()));
    registry.register_sink("postgres", Arc::new(PgSinkFactory::postgres()));
    registry.register_storage("postgres", Arc::new(PgStorageFactory::postgres()));
    // CockroachDB is a Postgres-wire-compatible engine; the same connector
    // crates serve it under a separate `type = "cockroachdb"` registry key
    // with `Dialect::Cockroach` selecting the few divergent code paths
    // (40001 retry, XML-type rejection).
    registry.register_source("cockroachdb", Arc::new(PgSourceFactory::cockroach()));
    registry.register_sink("cockroachdb", Arc::new(PgSinkFactory::cockroach()));
    registry.register_storage("cockroachdb", Arc::new(PgStorageFactory::cockroach()));
    registry.register_source("mysql", Arc::new(MySqlSourceFactory));
    registry.register_sink("mysql", Arc::new(MySqlSinkFactory));
    registry.register_storage("mysql", Arc::new(MySqlStorageFactory));
    registry.register_source("mongodb", Arc::new(MongoSourceFactory));
    registry.register_source("mongo-cdc", Arc::new(MongoCdcSourceFactory));
    registry.register_sink("mongodb", Arc::new(MongoSinkFactory));
    registry.register_storage("mongodb", Arc::new(MongoStorageFactory));
    // ClickHouse — sink only. The sink declares `supports_deletes() = false`
    // so the runner drops `RowOp::Delete` rows pre-write and CDC sources
    // (e.g. `mongo-cdc`) may pair with it without a mandatory
    // `[flow.x.conflict]` block (append-only ingest).
    registry.register_sink("clickhouse", Arc::new(ChSinkFactory));
    registry
}
