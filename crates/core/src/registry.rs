//! Connector-type → factory registry.
//!
//! Factories are async because opening a sqlx pool is async. Each factory is
//! a dedicated trait object (`Arc<dyn SourceFactory>`) with a single `async
//! fn build`. No `Box<dyn Fn -> Pin<Box<dyn Future>>>` juggling, no sync-to-
//! async thread-scope bridge — the registry is simply called from the
//! already-async validation pipeline.

use std::sync::Arc;

use ahash::AHashMap;

use async_trait::async_trait;

use crate::config::model::ComponentConfig;
use crate::config::validation::SamplingConfig;
use crate::error::{ConfigError, RuntimeError, RuntimeResult};
use crate::traits::{Sink, Source, Storage};

#[async_trait]
pub trait SourceFactory: Send + Sync {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Source>, ConfigError>;

    /// Per-backend default for the sampling-validation step.
    /// Off for SQL connectors (the schema is authoritative); on for
    /// MongoDB (no fixed schema). Operators can override either way
    /// via `[flow.<name>.validation.sampling]` in TOML.
    fn sampling_default(&self) -> SamplingConfig {
        SamplingConfig::Disabled
    }
}

#[async_trait]
pub trait SinkFactory: Send + Sync {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Sink>, ConfigError>;
}

#[async_trait]
pub trait StorageFactory: Send + Sync {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Storage>, ConfigError>;
}

#[derive(Default, Clone)]
pub struct Registry {
    sources: AHashMap<String, Arc<dyn SourceFactory>>,
    sinks: AHashMap<String, Arc<dyn SinkFactory>>,
    storages: AHashMap<String, Arc<dyn StorageFactory>>,
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

    pub async fn build_source(&self, cfg: &ComponentConfig) -> RuntimeResult<Box<dyn Source>> {
        let f = self
            .sources
            .get(&cfg.kind)
            .ok_or_else(|| RuntimeError::NotRegistered {
                component: format!("source:{}", cfg.kind),
            })?;
        f.build(cfg).await.map_err(RuntimeError::Config)
    }

    pub async fn build_sink(&self, cfg: &ComponentConfig) -> RuntimeResult<Box<dyn Sink>> {
        let f = self
            .sinks
            .get(&cfg.kind)
            .ok_or_else(|| RuntimeError::NotRegistered {
                component: format!("sink:{}", cfg.kind),
            })?;
        f.build(cfg).await.map_err(RuntimeError::Config)
    }

    /// Look up the per-backend sampling-validation default, given a
    /// source `kind` (e.g. `"postgres"`, `"mongodb"`). Returns
    /// `SamplingConfig::Disabled` if the kind is not registered —
    /// that case will surface as a clearer error elsewhere when the
    /// flow tries to build the source.
    pub fn sampling_default(&self, kind: &str) -> SamplingConfig {
        self.sources
            .get(kind)
            .map(|f| f.sampling_default())
            .unwrap_or(SamplingConfig::Disabled)
    }

    /// Sorted list of registered source `kind`s.
    pub fn source_kinds(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.sources.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Sorted list of registered sink `kind`s.
    pub fn sink_kinds(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.sinks.keys().cloned().collect();
        keys.sort();
        keys
    }

    /// Sorted list of registered storage `kind`s.
    pub fn storage_kinds(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.storages.keys().cloned().collect();
        keys.sort();
        keys
    }

    pub async fn build_storage(&self, cfg: &ComponentConfig) -> RuntimeResult<Box<dyn Storage>> {
        let f = self
            .storages
            .get(&cfg.kind)
            .ok_or_else(|| RuntimeError::NotRegistered {
                component: format!("storage:{}", cfg.kind),
            })?;
        f.build(cfg).await.map_err(RuntimeError::Config)
    }
}
