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
use tokio::sync::{OnceCell, watch};

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
    /// Tracks whether `Storage::migrate` has run **to completion** for
    /// this `App`. Flipped to `true` only after every storage's
    /// `migrate()` returned `Ok`; a partial failure (e.g. storage #2
    /// errors after #1 succeeded) leaves the flag at `false` so the
    /// next caller retries the full pass. The `migrate_lock` mutex
    /// below serialises concurrent attempts so we don't run the DDL
    /// twice when two callers race.
    migrated: AtomicBool,
    /// Serialises concurrent calls to `migrate()` / `run_once()` so
    /// only one task drives the DDL pass at a time. The atomic above
    /// is the "done" signal; this mutex is the "in flight" gate. The
    /// `Notify`-pattern alternative would be more elegant but the
    /// mutex is dead-simple and migrate is a once-per-process event.
    migrate_lock: tokio::sync::Mutex<()>,
    /// Lazily-populated cache of the assembled flows — the no-I/O stage
    /// of the validation pipeline that resolves component names, builds
    /// connector instances through the registry, and constructs the
    /// per-flow `ReadSpec`/`WriteSpec` halves. Filled on first call to
    /// `flows_assembled()` and reused by every subsequent operation on
    /// this `App`. Caching here (rather than re-running `assemble` per
    /// call) keeps the same `Arc<dyn Source/Sink/Storage>` — and
    /// therefore the same sqlx/mongo pools — alive across successive
    /// `migrate` / `validate` / `run_once` / `run_daemon` calls.
    assembled: OnceCell<Vec<AssembledFlow>>,
    /// Lazily-populated cache of the validated flows — the I/O stage of
    /// the validation pipeline (access probes, schema introspection,
    /// type matrix, optional sampling). Filled on first call to
    /// `flows_validated()`. Distinct from `assembled` because
    /// `migrate()` must be able to drive `Storage::migrate` on the
    /// assembled flows *before* any I/O probe runs — sampling-validation
    /// reads `air_elt_cursors` through the runner, which would fail on a
    /// fresh database whose storage tables don't exist yet.
    validated: OnceCell<Vec<FlowState>>,
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
            migrated: AtomicBool::new(false),
            migrate_lock: tokio::sync::Mutex::new(()),
            assembled: OnceCell::new(),
            validated: OnceCell::new(),
        }
    }

    /// Lazily run (and cache) the no-I/O `assemble` stage. Used by
    /// `migrate` (which must run `Storage::migrate` *before* any I/O
    /// probe) and by `flows_validated` (which feeds the assembled flows
    /// into the I/O stage). Caching means every subsequent operation on
    /// this `App` reuses the same connector `Arc`s — and therefore the
    /// same sqlx/mongo pools.
    async fn flows_assembled(&self) -> Result<&Vec<AssembledFlow>> {
        self.assembled
            .get_or_try_init(|| async {
                let assembled = assemble(&self.config, &self.registry).await?;
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

    /// Sorted lists of registered source / sink / storage kinds.
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

    /// Run `Storage::migrate` for every declared storage. Only the
    /// no-I/O `assemble` stage runs first — the validation I/O probes
    /// are deferred to the runner caller. This ordering is load-bearing:
    /// sampling-validation reads from `air_elt_cursors` through the
    /// runner, so running it before the storage tables exist would
    /// surface as `relation "air_elt_cursors" does not exist`. After
    /// this returns, the `migrated` flag is set so `run_once` won't
    /// repeat the DDL.
    pub fn migrate(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let assembled = self.flows_assembled().await?;
            // Fast path: already done. Avoids taking the mutex on
            // steady-state callers (e.g. multiple `run_once()` in a
            // row).
            if self.migrated.load(Ordering::Acquire) {
                return Ok(());
            }
            let _guard = self.migrate_lock.lock().await;
            // Re-check inside the lock — a racing caller may have
            // completed the pass while we were waiting.
            if self.migrated.load(Ordering::Acquire) {
                return Ok(());
            }
            for f in assembled {
                f.storage.migrate().await?;
            }
            // Flag flips ONLY after every migrate() returned Ok.
            // A mid-pass failure leaves the flag at false, so the
            // next caller retries the whole DDL pass against the
            // remaining unmigrated storages.
            self.migrated.store(true, Ordering::Release);
            Ok(())
        })
    }

    /// Run all flows once (drain + exit). Mirrors `air-elt run --once`.
    /// Storage migrations are applied (if not already) before the I/O
    /// validation pass so its cursor-table probes succeed; subsequent
    /// calls (including an earlier explicit `migrate()`) skip the DDL.
    pub fn run_once(&self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            // Reuse the dedicated migrate() path so the success-only
            // flag flip + mutex serialisation logic lives in exactly
            // one place.
            self.migrate().await?;
            let flows = self.flows_validated().await?;
            let (_tx, rx) = watch::channel(false);
            FlowEngine::new(flows.clone(), RunMode::Once, rx)
                .run()
                .await?;
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
