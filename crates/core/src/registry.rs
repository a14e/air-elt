//! Connector-type → factory registry.
//!
//! Factories are async because opening a sqlx pool is async. Each factory is
//! a dedicated trait object (`Arc<dyn SourceFactory>`) with a single `async
//! fn build`. No `Box<dyn Fn -> Pin<Box<dyn Future>>>` juggling, no sync-to-
//! async thread-scope bridge — the registry is simply called from the
//! already-async validation pipeline.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;

use crate::config::model::ComponentConfig;
use crate::error::{ConfigError, RuntimeError, RuntimeResult};
use crate::traits::{Sink, Source, Storage};

#[async_trait]
pub trait SourceFactory: Send + Sync {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Arc<dyn Source>, ConfigError>;
}

#[async_trait]
pub trait SinkFactory: Send + Sync {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Arc<dyn Sink>, ConfigError>;
}

#[async_trait]
pub trait StorageFactory: Send + Sync {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Arc<dyn Storage>, ConfigError>;
}

#[derive(Default, Clone)]
pub struct Registry {
    sources: HashMap<String, Arc<dyn SourceFactory>>,
    sinks: HashMap<String, Arc<dyn SinkFactory>>,
    storages: HashMap<String, Arc<dyn StorageFactory>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_source(&mut self, kind: &str, factory: Arc<dyn SourceFactory>) {
        self.sources.insert(kind.to_string(), factory);
    }

    pub fn register_sink(&mut self, kind: &str, factory: Arc<dyn SinkFactory>) {
        self.sinks.insert(kind.to_string(), factory);
    }

    pub fn register_storage(&mut self, kind: &str, factory: Arc<dyn StorageFactory>) {
        self.storages.insert(kind.to_string(), factory);
    }

    pub async fn build_source(&self, cfg: &ComponentConfig) -> RuntimeResult<Arc<dyn Source>> {
        let f = self
            .sources
            .get(&cfg.kind)
            .ok_or_else(|| RuntimeError::NotRegistered {
                component: format!("source:{}", cfg.kind),
            })?;
        f.build(cfg).await.map_err(RuntimeError::Config)
    }

    pub async fn build_sink(&self, cfg: &ComponentConfig) -> RuntimeResult<Arc<dyn Sink>> {
        let f = self
            .sinks
            .get(&cfg.kind)
            .ok_or_else(|| RuntimeError::NotRegistered {
                component: format!("sink:{}", cfg.kind),
            })?;
        f.build(cfg).await.map_err(RuntimeError::Config)
    }

    pub async fn build_storage(&self, cfg: &ComponentConfig) -> RuntimeResult<Arc<dyn Storage>> {
        let f = self
            .storages
            .get(&cfg.kind)
            .ok_or_else(|| RuntimeError::NotRegistered {
                component: format!("storage:{}", cfg.kind),
            })?;
        f.build(cfg).await.map_err(RuntimeError::Config)
    }
}
