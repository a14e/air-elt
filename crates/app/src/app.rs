//! Embeddable wrapper around the air-elt CLI commands.
//!
//! `App` owns a loaded `RootConfig` plus a built `Registry`, and exposes
//! the same operations the CLI does — `migrate`, `validate`, `run_once`,
//! `run_daemon`, `list_kinds` — so the binary entrypoint and integration
//! tests share a single codegen point and the wiring logic doesn't drift.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use anyhow::Result;
use tokio::sync::watch;

use air_elt_core::config::loader;
use air_elt_core::config::model::RootConfig;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::registry::Registry;
use air_elt_core::validation::pipeline::{assemble, validate};

use crate::registry::build_registry;

/// Snapshot of the connector kinds wired into a built `Registry`. Returned
/// by `App::list_kinds` and rendered by the `list-kinds` CLI subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedKinds {
    pub sources: Vec<String>,
    pub sinks: Vec<String>,
    pub storages: Vec<String>,
}

/// Loaded config plus a pre-built registry. Constructed once per process
/// run; every operation borrows `&self` so callers can freely chain
/// `migrate().await?; run_once().await?;` without re-parsing TOML.
pub struct App {
    config: RootConfig,
    registry: Registry,
}

impl App {
    /// Load the config from disk and wire the default registry.
    pub fn from_path(path: &Path) -> Result<Self> {
        let config = loader::load(path)?;
        Ok(Self::from_config(config))
    }

    /// Wrap an already-loaded config (used by tests that build configs in
    /// memory). Wires the default registry.
    pub fn from_config(config: RootConfig) -> Self {
        Self {
            config,
            registry: build_registry(),
        }
    }

    /// Sorted lists of registered source / sink / storage kinds.
    pub fn list_kinds(&self) -> ListedKinds {
        ListedKinds {
            sources: self.registry.source_kinds(),
            sinks: self.registry.sink_kinds(),
            storages: self.registry.storage_kinds(),
        }
    }

    /// Run the full validation pipeline (assemble + validate). I/O probes
    /// against every declared connector.
    pub fn validate(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let assembled = assemble(&self.config, &self.registry).await?;
            let _flows = validate(assembled).await?;
            Ok(())
        })
    }

    /// Run `Storage::migrate` for every declared storage. Performs an
    /// initial assemble + validate so storage handles exist.
    pub fn migrate(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let assembled = assemble(&self.config, &self.registry).await?;
            let flows = validate(assembled).await?;
            for f in &flows {
                f.storage.migrate().await?;
            }
            Ok(())
        })
    }

    /// Run all flows once (drain + exit). Mirrors `air-elt run --once`.
    /// The pipeline is assembled twice intentionally: the first pass exists
    /// solely to produce `Storage` handles for `migrate`, so the second
    /// pass's access probes land against the migrated cursor table.
    pub fn run_once(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.migrate().await?;
            let assembled = assemble(&self.config, &self.registry).await?;
            let flows = validate(assembled).await?;
            let (_tx, rx) = watch::channel(false);
            FlowEngine::new(flows, RunMode::Once, rx).run().await?;
            Ok(())
        })
    }

    /// Run all flows in daemon mode until `shutdown` flips to `true`.
    /// Storage migrations are NOT run here — call `migrate()` first if the
    /// storage tables may not yet exist.
    pub fn run_daemon(
        &self,
        shutdown: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let assembled = assemble(&self.config, &self.registry).await?;
            let flows = validate(assembled).await?;
            FlowEngine::new(flows, RunMode::Daemon, shutdown)
                .run()
                .await?;
            Ok(())
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        // CARGO_MANIFEST_DIR for this crate is `crates/app`.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn from_path_loads_example_config() {
        let cfg_path = workspace_root().join("examples/pg-to-cockroachdb/config.toml");
        let app = App::from_path(&cfg_path).expect("from_path");
        assert!(!app.config.sources.is_empty());
        assert!(!app.config.sinks.is_empty());
    }

    #[test]
    fn from_config_uses_default_registry() {
        let cfg_path = workspace_root().join("examples/pg-to-cockroachdb/config.toml");
        let raw = loader::load(&cfg_path).expect("load");
        let app = App::from_config(raw);
        let kinds = app.list_kinds();
        assert!(kinds.sources.contains(&"postgres".to_string()));
    }

    #[test]
    fn list_kinds_contains_all_registered_kinds() {
        let cfg_path = workspace_root().join("examples/pg-to-cockroachdb/config.toml");
        let app = App::from_path(&cfg_path).expect("from_path");
        let kinds = app.list_kinds();

        for k in ["postgres", "mysql", "mongodb", "mongo-cdc", "cockroachdb"] {
            assert!(
                kinds.sources.contains(&k.to_string()),
                "missing source kind: {k}"
            );
        }
        for k in ["postgres", "mysql", "mongodb", "cockroachdb"] {
            assert!(
                kinds.sinks.contains(&k.to_string()),
                "missing sink kind: {k}"
            );
            assert!(
                kinds.storages.contains(&k.to_string()),
                "missing storage kind: {k}"
            );
        }
    }
}
