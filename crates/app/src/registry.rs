//! Wire the three first-party connector factories into a fresh `Registry`.
//!
//! Factory structs are zero-sized — they exist only to dispatch to
//! `PgSource::connect` / `PgSink::connect` / `PgStorage::connect`. The
//! registry is the single point where downstream code (validation, runner)
//! obtains component instances.

use std::sync::Arc;

use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::{Registry, SinkFactory, SourceFactory, StorageFactory};
use air_elt_core::traits::{Sink, Source, Storage};
use air_elt_sink_postgres::{PgSink, PgSinkConfig};
use air_elt_source_postgres::{PgSource, PgSourceConfig};
use air_elt_storage_postgres::{PgStorage, PgStorageConfig};

pub fn build_registry() -> Registry {
    let mut registry = Registry::new();
    registry.register_source("postgres", Arc::new(PgSourceFactory));
    registry.register_sink("postgres", Arc::new(PgSinkFactory));
    registry.register_storage("postgres", Arc::new(PgStorageFactory));
    registry
}

struct PgSourceFactory;

#[async_trait]
impl SourceFactory for PgSourceFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Arc<dyn Source>, ConfigError> {
        let config = PgSourceConfig::try_from(cfg)?;
        let source = PgSource::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Arc::new(source))
    }
}

struct PgSinkFactory;

#[async_trait]
impl SinkFactory for PgSinkFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Arc<dyn Sink>, ConfigError> {
        let config = PgSinkConfig::try_from(cfg)?;
        let sink = PgSink::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Arc::new(sink))
    }
}

struct PgStorageFactory;

#[async_trait]
impl StorageFactory for PgStorageFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Arc<dyn Storage>, ConfigError> {
        let config = PgStorageConfig::try_from(cfg)?;
        let storage = PgStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Arc::new(storage))
    }
}
