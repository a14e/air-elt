//! Embeddable wrapper around the air-elt CLI commands.
//!
//! `App` owns a loaded `RootConfig` plus a built `Registry`, and exposes
//! the same operations the CLI does — `migrate`, `validate`, `run_once`,
//! `run_daemon`, `list_kinds` — so the binary entrypoint and integration
//! tests share a single codegen point and the wiring logic doesn't drift.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::Result;
use parking_lot::Mutex;
use tokio::sync::{OnceCell, watch};
use tracing::error;

use air_elt_core::config::loader;
use air_elt_core::config::model::RootConfig;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::model::{AssembledFlow, FlowState};
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
    /// Directory containing the root config file. Used to build the
    /// expression evaluation context (for resolving relative file
    /// paths in expressions).
    config_dir: std::path::PathBuf,
    /// Construction-time monitoring handle. Wrapped in a parking_lot
    /// `Mutex<Option<…>>` to keep interior mutability under `&self`
    /// while honouring the constraint that the manager itself has no
    /// internal mutex. Lifecycle: built once at construction, taken
    /// out by [`Self::spawn_metrics`] when the metrics server starts.
    /// `MetricsScraper` (cloned post-take) is what the HTTP server task
    /// uses; the assemble pipeline locks briefly to mint recorders.
    monitoring: Mutex<Option<air_elt_monitoring::MonitoringManager>>,
    /// Tracks whether `Storage::migrate` has run **to completion** for
    /// this `App`. Flipped to `true` only after every storage's
    /// `migrate()` returned `Ok`; a partial failure (e.g. storage #2
    /// errors after #1 succeeded) leaves the flag at `false` so the
    /// next caller retries the full pass.
    migrated: AtomicBool,
    migrate_lock: tokio::sync::Mutex<()>,
    assembled: OnceCell<Vec<AssembledFlow>>,
    validated: OnceCell<Vec<FlowState>>,
}

/// RAII guard that borrows the `MonitoringManager` out of `App`'s
/// `Mutex<Option<…>>` for the duration of an `assemble` call. On drop
/// (normal or unwind) the manager is restored to the slot — preventing
/// a panic / cancellation mid-assemble from silently disabling metrics
/// for the rest of the process.
struct MonitoringGuard<'a> {
    slot: &'a Mutex<Option<air_elt_monitoring::MonitoringManager>>,
    inner: Option<air_elt_monitoring::MonitoringManager>,
}

impl<'a> MonitoringGuard<'a> {
    fn take(slot: &'a Mutex<Option<air_elt_monitoring::MonitoringManager>>) -> Self {
        let inner = match slot.lock().take() {
            Some(inner) => inner,
            None => {
                tracing::warn!(
                    "monitoring guard taken twice — metrics disabled for the rest of the process \
                     (concurrent assemble or post-spawn_metrics access; this is a wiring bug)"
                );
                air_elt_monitoring::MonitoringManager::disabled()
            }
        };
        Self {
            slot,
            inner: Some(inner),
        }
    }

    fn as_mut(&mut self) -> &mut air_elt_monitoring::MonitoringManager {
        self.inner
            .as_mut()
            .expect("inner stays Some until Drop runs")
    }
}

impl Drop for MonitoringGuard<'_> {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            *self.slot.lock() = Some(inner);
        }
    }
}

impl App {
    /// Load the config from disk and wire the default registry.
    pub fn from_path(path: &Path) -> Result<Self> {
        let config = loader::load(path)?;
        let config_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok(Self::from_config_with_dir(config, config_dir))
    }

    /// Wrap an already-loaded config (used by tests that build configs in
    /// memory). Wires the default registry. Uses cwd as config_dir.
    pub fn from_config(config: RootConfig) -> Self {
        Self::from_config_with_dir(config, std::path::PathBuf::from("."))
    }

    /// Wrap an already-loaded config with an explicit config directory
    /// for expression resolution.
    fn from_config_with_dir(config: RootConfig, config_dir: std::path::PathBuf) -> Self {
        let mut monitoring = match config
            .metrics
            .prometheus
            .clone()
            .map(air_elt_monitoring::MonitoringManager::new)
        {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                error!(error = %e, "failed to build monitoring manager; metrics disabled");
                air_elt_monitoring::MonitoringManager::disabled()
            }
            None => air_elt_monitoring::MonitoringManager::disabled(),
        };
        monitoring.set_counts(
            config.flow.len() as u32,
            config.sources.len() as u32,
            config.sinks.len() as u32,
            config.storages.len() as u32,
        );
        Self {
            config,
            registry: build_registry(),
            config_dir,
            monitoring: Mutex::new(Some(monitoring)),
            migrated: AtomicBool::new(false),
            migrate_lock: tokio::sync::Mutex::new(()),
            assembled: OnceCell::new(),
            validated: OnceCell::new(),
        }
    }

    pub fn is_monitoring_enabled(&self) -> bool {
        self.monitoring
            .lock()
            .as_ref()
            .is_some_and(|m| m.is_enabled())
    }

    /// Spawn the metrics HTTP server task, returning the `JoinHandle`.
    /// Consumes the `MonitoringManager` (the recorder cache is no
    /// longer needed) and freezes it into a `MetricsScraper` shared
    /// with the server task. Returns `None` when monitoring is
    /// disabled or has already been spawned.
    pub fn spawn_metrics(
        &self,
        shutdown: watch::Receiver<bool>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let manager = self.monitoring.lock().take()?;
        if !manager.is_enabled() {
            return None;
        }
        let scraper = manager.into_scraper();
        Some(tokio::spawn(async move {
            if let Err(e) = air_elt_monitoring::server::serve(scraper, shutdown).await {
                error!(error = %e, "metrics server stopped with error");
            }
        }))
    }

    /// Consume the monitoring manager and return the scrape-time
    /// handle. Used by integration tests that bind their own ephemeral
    /// `TcpListener` and spawn `air_elt_monitoring::server::serve_on_listener`
    /// directly — production code goes through [`Self::spawn_metrics`]
    /// instead.
    pub fn take_scraper(&self) -> air_elt_monitoring::MetricsScraper {
        self.monitoring
            .lock()
            .take()
            .map(air_elt_monitoring::MonitoringManager::into_scraper)
            .unwrap_or_default()
    }

    /// Lazily run (and cache) the no-I/O `assemble` stage.
    async fn flows_assembled(&self) -> Result<&Vec<AssembledFlow>> {
        self.assembled
            .get_or_try_init(|| async {
                // `MonitoringGuard` borrows the manager out of the
                // `Mutex<Option<…>>`, hands `&mut` to assemble, and
                // restores it on Drop — including on panic or future
                // cancellation between the `.take()` and the manual
                // put-back below. Without it a mid-assemble unwind
                // would leave the manager `None` and silently disable
                // `/metrics` for the rest of the process lifetime.
                let mut guard = MonitoringGuard::take(&self.monitoring);
                let assembled = assemble(
                    &self.config,
                    &self.registry,
                    guard.as_mut(),
                    Some(&self.config_dir),
                )
                .await?;
                Ok::<_, anyhow::Error>(assembled)
            })
            .await
    }

    /// Lazily run (and cache) the full validation pipeline. Builds on
    /// the cached `flows_assembled` output and runs the I/O stage
    /// (access probes, schema introspection, type matrix, optional
    /// sampling). Callers that depend on storage tables existing (i.e.
    /// `run_once`, `run_daemon`) MUST have run `migrate()` first — the
    /// sampling-validation path issues `SELECT … FROM air_elt_cursors`
    /// through the runner, which fails on a fresh database without the
    /// storage migrations applied.
    async fn flows_validated(&self) -> Result<&Vec<FlowState>> {
        self.validated
            .get_or_try_init(|| async {
                let assembled = self.flows_assembled().await?.clone();
                let flows = validate(assembled).await?;
                Ok::<_, anyhow::Error>(flows)
            })
            .await
    }

    pub fn list_kinds(&self) -> ListedKinds {
        ListedKinds {
            sources: self.registry.source_kinds(),
            sinks: self.registry.sink_kinds(),
            storages: self.registry.storage_kinds(),
        }
    }

    /// Run the full validation pipeline (assemble + validate). I/O probes
    /// against every declared connector. Sampling-validation (when
    /// enabled) drives a dry-run runner tick that reads from
    /// `air_elt_cursors`, so callers operating against a fresh database
    /// must call `migrate()` first.
    pub fn validate(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.flows_validated().await?;
            Ok(())
        })
    }

    /// Run only the no-I/O assemble stage and cache the result. Used by
    /// the CLI to mint recorders into `AssembledFlow` BEFORE spawning
    /// the metrics server, so a subsequent validate I/O failure has a
    /// live `/metrics` endpoint to surface error counters through.
    pub fn assemble(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.flows_assembled().await?;
            Ok(())
        })
    }

    pub fn migrate(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let assembled = self.flows_assembled().await?;
            if self.migrated.load(Ordering::Acquire) {
                return Ok(());
            }
            let _guard = self.migrate_lock.lock().await;
            if self.migrated.load(Ordering::Acquire) {
                return Ok(());
            }
            for f in assembled {
                f.storage.migrate().await?;
            }
            self.migrated.store(true, Ordering::Release);
            Ok(())
        })
    }

    pub fn run_once(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            self.migrate().await?;
            let flows = self.flows_validated().await?;
            let (_tx, rx) = watch::channel(false);
            FlowEngine::new(flows.clone(), RunMode::Once, rx)
                .run()
                .await?;
            Ok(())
        })
    }

    pub fn run_daemon(
        &self,
        shutdown: watch::Receiver<bool>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let flows = self.flows_validated().await?;
            FlowEngine::new(flows.clone(), RunMode::Daemon, shutdown)
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
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    /// `MonitoringGuard` must restore the manager on Drop even when
    /// the caller never explicitly returns it. Simulates the failure
    /// path: take the guard, drop it without using `as_mut`, and
    /// confirm the slot's `Option` is still `Some`.
    #[test]
    fn monitoring_guard_restores_on_drop() {
        let slot = Mutex::new(Some(air_elt_monitoring::MonitoringManager::disabled()));
        {
            let _guard = MonitoringGuard::take(&slot);
            assert!(
                slot.lock().is_none(),
                "manager moved out of slot for the guard's lifetime"
            );
        }
        assert!(
            slot.lock().is_some(),
            "manager restored on Drop — assemble panic / cancellation cannot silently disable metrics"
        );
    }

    #[test]
    fn from_path_loads_example_config() {
        let cfg_path = workspace_root().join("examples/pg-to-cockroachdb/config.yml");
        let app = App::from_path(&cfg_path).expect("from_path");
        assert!(!app.config.sources.is_empty());
        assert!(!app.config.sinks.is_empty());
    }

    #[test]
    fn from_config_uses_default_registry() {
        let cfg_path = workspace_root().join("examples/pg-to-cockroachdb/config.yml");
        let raw = loader::load(&cfg_path).expect("load");
        let app = App::from_config(raw);
        let kinds = app.list_kinds();
        assert!(kinds.sources.contains(&"postgres".to_string()));
    }

    #[test]
    fn list_kinds_contains_all_registered_kinds() {
        let cfg_path = workspace_root().join("examples/pg-to-cockroachdb/config.yml");
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
