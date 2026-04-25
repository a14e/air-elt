//! Wire connector factories into a fresh `Registry`.

use std::sync::Arc;

use air_elt_core::registry::Registry;
use air_elt_sink_postgres::PgSinkFactory;
use air_elt_source_postgres::PgSourceFactory;
use air_elt_storage_postgres::PgStorageFactory;

pub fn build_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_source("postgres", Arc::new(PgSourceFactory));
    registry.register_sink("postgres", Arc::new(PgSinkFactory));
    registry.register_storage("postgres", Arc::new(PgStorageFactory));
    registry
}
