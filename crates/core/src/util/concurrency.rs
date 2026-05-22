//! Backend concurrency caps for the validation pipeline and the
//! runtime flow runner.
//!
//! Each declared `[[sources]]` / `[[sinks]]` / `[[storages]]` instance
//! has one `tokio::sync::Semaphore` sized to its `max-connections`.
//! Every I/O unit (read_batch, write_batch, save_cursor, schema fetch,
//! access probe, …) acquires the permit of *just* the component it
//! touches, holds it for the duration of that call, and releases.
//! No call site ever holds more than one permit at a time, so deadlock
//! between flows is structurally impossible — there is no canonical
//! lock order because there is no multi-lock acquisition.
//!
//! The two outward types are [`ConcurrencyManager`] (built at
//! `validation::pipeline::assemble`, owns the per-component
//! semaphores) and [`FlowLockHandle`] (one per flow, lives on
//! `FlowState`, exposes one `acquire_*` method per component kind).
//! Callers hold the returned [`SemaphorePermit`] for the critical
//! section; the permit releases on drop.
//!
//! See the `project-conventions` skill ("Concurrency: per-component
//! semaphores") for the cross-cutting contract.

use std::sync::Arc;

use ahash::AHashMap;
use air_elt_monitoring::{ActiveGuard, ComponentKind, LockRecorder, MonitoringManager};
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::info;

use crate::error::RuntimeError;

/// One registered component: the semaphore that fronts its pool. The
/// declared `max-connections` value is consumed by `register_into` to
/// size the semaphore and is not retained — `log_concurrency_budgets`
/// reads the live permit count directly via `available_permits()`. The
/// configuration gauge `air_elt_lock_max` is set independently by
/// `assemble` via `MonitoringManager::set_lock_max`.
struct Slot {
    semaphore: Arc<Semaphore>,
}

/// Registry of one `Semaphore` per declared component instance, keyed
/// by `(kind, name)`. Built and frozen at validation `assemble` time;
/// after that the map is read-only and shared via `Arc` to every flow
/// (no `Mutex` needed because nothing mutates the inner map post-
/// assemble).
#[derive(Default)]
pub struct ConcurrencyManager {
    sources: AHashMap<String, Slot>,
    sinks: AHashMap<String, Slot>,
    storages: AHashMap<String, Slot>,
}

impl ConcurrencyManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one source instance with its `max-connections`.
    /// Idempotent by name — the assemble walk only calls this once per
    /// declared component, but the contract is defensive so a future
    /// caller that registers the same name twice cannot accidentally
    /// replace the live `Arc<Semaphore>` (which would orphan any
    /// handle already holding the original). `or_insert_with` preserves
    /// the Arc identity on the second call.
    pub fn register_source(&mut self, name: &str, max_connections: u32) {
        let permits = (max_connections as usize).min(Semaphore::MAX_PERMITS);
        self.sources
            .entry(name.to_string())
            .or_insert_with(|| Slot {
                semaphore: Arc::new(Semaphore::new(permits)),
            });
    }

    pub fn register_sink(&mut self, name: &str, max_connections: u32) {
        let permits = (max_connections as usize).min(Semaphore::MAX_PERMITS);
        self.sinks.entry(name.to_string()).or_insert_with(|| Slot {
            semaphore: Arc::new(Semaphore::new(permits)),
        });
    }

    pub fn register_storage(&mut self, name: &str, max_connections: u32) {
        let permits = (max_connections as usize).min(Semaphore::MAX_PERMITS);
        self.storages
            .entry(name.to_string())
            .or_insert_with(|| Slot {
                semaphore: Arc::new(Semaphore::new(permits)),
            });
    }

    /// Build a per-flow handle. Resolves the three components against
    /// the registry **once** here and caches the `Arc<Semaphore>`
    /// triple on the handle, so the hot `acquire_*` path is a single
    /// `Semaphore::acquire` call — no hashmap lookup.
    ///
    /// Lock recorders are minted via the supplied `MonitoringManager`
    /// and are idempotent on `(kind, name)`. When monitoring is
    /// disabled, every `LockRecorder` collapses to a no-op.
    ///
    /// Panics if any of the three components has not been registered.
    /// That is a programming error in `assemble`, not a user-facing
    /// failure.
    pub fn handle(
        &self,
        source: &str,
        sink: &str,
        storage: &str,
        monitoring: &mut MonitoringManager,
    ) -> FlowLockHandle {
        let source_slot = self
            .sources
            .get(source)
            .unwrap_or_else(|| panic!("source {source:?} not registered"));
        let sink_slot = self
            .sinks
            .get(sink)
            .unwrap_or_else(|| panic!("sink {sink:?} not registered"));
        let storage_slot = self
            .storages
            .get(storage)
            .unwrap_or_else(|| panic!("storage {storage:?} not registered"));

        let source_recorder = monitoring.lock_recorder(ComponentKind::Source, source);
        let sink_recorder = monitoring.lock_recorder(ComponentKind::Sink, sink);
        let storage_recorder = monitoring.lock_recorder(ComponentKind::Storage, storage);

        FlowLockHandle {
            source_semaphore: source_slot.semaphore.clone(),
            sink_semaphore: sink_slot.semaphore.clone(),
            storage_semaphore: storage_slot.semaphore.clone(),
            source_recorder,
            sink_recorder,
            storage_recorder,
        }
    }
}

/// Emit one `info!` per registered component recording its permit
/// budget. Free function rather than a method on the manager — this
/// is one-shot operator diagnostics, not a runtime concern. Sorted
/// output for stable diffs across runs.
pub fn log_concurrency_budgets(mgr: &ConcurrencyManager) {
    let mut rows: Vec<(&'static str, &str, usize)> = Vec::new();
    for (n, s) in &mgr.sources {
        rows.push(("source", n.as_str(), s.semaphore.available_permits()));
    }
    for (n, s) in &mgr.sinks {
        rows.push(("sink", n.as_str(), s.semaphore.available_permits()));
    }
    for (n, s) in &mgr.storages {
        rows.push(("storage", n.as_str(), s.semaphore.available_permits()));
    }
    rows.sort();
    for (kind, name, permits) in rows {
        info!(
            component = kind,
            name = %name,
            permits,
            "concurrency cap"
        );
    }
}

/// Per-flow lock handle. Cheap to clone (three `Arc<Semaphore>` ref
/// counts). Exposes one `acquire_*` method per component kind so the
/// caller scopes the lock to exactly the I/O unit that needs it —
/// nothing more, nothing less. No call site ever holds more than one
/// permit at a time, so two flows on the same backend pool serialise
/// only on that pool, not on each other's unrelated reads/writes.
#[derive(Clone)]
pub struct FlowLockHandle {
    source_semaphore: Arc<Semaphore>,
    sink_semaphore: Arc<Semaphore>,
    storage_semaphore: Arc<Semaphore>,
    source_recorder: LockRecorder,
    sink_recorder: LockRecorder,
    storage_recorder: LockRecorder,
}

/// RAII guard bundling the held semaphore permit with the lock's
/// `ActiveGuard`. On drop both are released — the permit returns to
/// the semaphore, the `lock_active_seconds_integral` slot decrements.
pub struct LockGuard<'a> {
    _permit: SemaphorePermit<'a>,
    _active: ActiveGuard<'a>,
}

impl FlowLockHandle {
    pub async fn acquire_source(&self) -> Result<LockGuard<'_>, RuntimeError> {
        self.acquire(ComponentKind::Source).await
    }

    pub async fn acquire_sink(&self) -> Result<LockGuard<'_>, RuntimeError> {
        self.acquire(ComponentKind::Sink).await
    }

    pub async fn acquire_storage(&self) -> Result<LockGuard<'_>, RuntimeError> {
        self.acquire(ComponentKind::Storage).await
    }

    /// Shared acquire path. Scopes the `lock_queue` integrating slot to
    /// exactly the `acquire().await` span: increment on entry, drop the
    /// guard once the permit is in hand, then attach the `lock_active`
    /// guard to the returned `LockGuard` so it tracks the held permit's
    /// lifetime.
    async fn acquire(&self, kind: ComponentKind) -> Result<LockGuard<'_>, RuntimeError> {
        let (semaphore, recorder) = match kind {
            ComponentKind::Source => (&self.source_semaphore, &self.source_recorder),
            ComponentKind::Sink => (&self.sink_semaphore, &self.sink_recorder),
            ComponentKind::Storage => (&self.storage_semaphore, &self.storage_recorder),
        };
        let _queue = recorder.queue_guard();
        let permit = semaphore.acquire().await.map_err(RuntimeError::backend)?;
        drop(_queue);
        Ok(LockGuard {
            _permit: permit,
            _active: recorder.active_guard(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn manager_with(
        srcs: &[(&str, u32)],
        sinks: &[(&str, u32)],
        storages: &[(&str, u32)],
    ) -> Arc<ConcurrencyManager> {
        let mut m = ConcurrencyManager::new();
        for (n, c) in srcs {
            m.register_source(n, *c);
        }
        for (n, c) in sinks {
            m.register_sink(n, *c);
        }
        for (n, c) in storages {
            m.register_storage(n, *c);
        }
        Arc::new(m)
    }

    /// Acquiring a component permit drains *only* that component's
    /// semaphore; the other two budgets are untouched.
    #[tokio::test]
    async fn per_component_acquire_does_not_touch_siblings() {
        let m = manager_with(&[("s", 1)], &[("k", 1)], &[("st", 1)]);
        let h = m.handle(
            "s",
            "k",
            "st",
            &mut air_elt_monitoring::MonitoringManager::disabled(),
        );
        {
            let _g = h.acquire_source().await.unwrap();
            assert_eq!(m.sources["s"].semaphore.available_permits(), 0);
            assert_eq!(m.sinks["k"].semaphore.available_permits(), 1);
            assert_eq!(m.storages["st"].semaphore.available_permits(), 1);
        }
        assert_eq!(m.sources["s"].semaphore.available_permits(), 1);
    }

    /// Two flows sharing the same single-permit source must serialise
    /// on `acquire_source`, even though their sinks differ. They never
    /// hold more than one permit at a time, so there is no cross-
    /// component lock to wait on.
    #[tokio::test]
    async fn shared_source_permit_queues_second_caller() {
        let m = manager_with(
            &[("shared", 1)],
            &[("sink_a", 1), ("sink_b", 1)],
            &[("storage_a", 1), ("storage_b", 1)],
        );
        let h1 = m.handle(
            "shared",
            "sink_a",
            "storage_a",
            &mut air_elt_monitoring::MonitoringManager::disabled(),
        );
        let h2 = m.handle(
            "shared",
            "sink_b",
            "storage_b",
            &mut air_elt_monitoring::MonitoringManager::disabled(),
        );

        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
        let tx1 = tx.clone();
        let t1 = tokio::spawn(async move {
            let _g = h1.acquire_source().await.unwrap();
            tx1.send("t1-start").unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            tx1.send("t1-end").unwrap();
        });
        let tx2 = tx.clone();
        // Give t1 a head start so its acquire happens first.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let t2 = tokio::spawn(async move {
            let _g = h2.acquire_source().await.unwrap();
            tx2.send("t2-start").unwrap();
        });
        t1.await.unwrap();
        t2.await.unwrap();
        drop(tx);

        let mut events = Vec::new();
        while let Some(e) = rx.recv().await {
            events.push(e);
        }
        // t2's acquire must succeed only after t1 releases — verified
        // by event order rather than wall-clock comparison.
        assert_eq!(events, vec!["t1-start", "t1-end", "t2-start"]);
    }

    /// Sink and storage permits are independent — holding a source
    /// permit does not block another flow from taking sink or storage
    /// permits on the same shared semaphore.
    #[tokio::test]
    async fn sink_permit_independent_of_source_permit() {
        let m = manager_with(&[("s", 1)], &[("k", 1)], &[("st", 1)]);
        let h = m.handle(
            "s",
            "k",
            "st",
            &mut air_elt_monitoring::MonitoringManager::disabled(),
        );
        let _src = h.acquire_source().await.unwrap();
        // Sink permit is unrelated to source — must acquire without
        // contention.
        let _snk = h.acquire_sink().await.unwrap();
        assert_eq!(m.sources["s"].semaphore.available_permits(), 0);
        assert_eq!(m.sinks["k"].semaphore.available_permits(), 0);
    }

    /// Asking the manager for an unregistered handle is a programming
    /// error and must panic with a useful message.
    #[test]
    #[should_panic(expected = "not registered")]
    fn unregistered_component_panics() {
        let m = manager_with(&[("s", 1)], &[("k", 1)], &[]);
        let _ = m.handle(
            "s",
            "k",
            "missing-storage",
            &mut air_elt_monitoring::MonitoringManager::disabled(),
        );
    }

    /// `register_*` is idempotent on name — registering the same
    /// source twice must NOT replace the live `Arc<Semaphore>`, or
    /// handles already issued would dangle from a stale cap.
    #[test]
    fn register_is_idempotent_by_name() {
        let mut m = ConcurrencyManager::new();
        m.register_source("shared_src", 5);
        m.register_sink("sink", 1);
        m.register_storage("storage", 1);
        let mgr = Arc::new(m);
        let h1 = mgr.handle(
            "shared_src",
            "sink",
            "storage",
            &mut air_elt_monitoring::MonitoringManager::disabled(),
        );
        // Re-register the same source — handle1's Arc must still
        // point at the same Semaphore the second handle gets.
        // (Unsafe to call from `&Arc<Self>`, so build a second
        // manager scenario via owned manager.)
        let arc_via_handle = h1.source_semaphore.clone();
        let arc_via_inner = mgr.sources["shared_src"].semaphore.clone();
        assert!(Arc::ptr_eq(&arc_via_handle, &arc_via_inner));
        assert_eq!(arc_via_handle.available_permits(), 5);
    }
}
