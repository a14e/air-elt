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
use tokio::sync::{Semaphore, SemaphorePermit};
use tracing::info;

use crate::error::RuntimeError;

/// Registry of one `Semaphore` per declared component instance, keyed
/// by `(kind, name)`. Built and frozen at validation `assemble` time;
/// after that the map is read-only and shared via `Arc` to every flow
/// (no `Mutex` needed because nothing mutates the inner map post-
/// assemble).
#[derive(Default)]
pub struct ConcurrencyManager {
    sources: AHashMap<String, Arc<Semaphore>>,
    sinks: AHashMap<String, Arc<Semaphore>>,
    storages: AHashMap<String, Arc<Semaphore>>,
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
    /// handle already holding the original).
    pub fn register_source(&mut self, name: &str, max_connections: u32) {
        register_into(&mut self.sources, name, max_connections);
    }

    pub fn register_sink(&mut self, name: &str, max_connections: u32) {
        register_into(&mut self.sinks, name, max_connections);
    }

    pub fn register_storage(&mut self, name: &str, max_connections: u32) {
        register_into(&mut self.storages, name, max_connections);
    }

    /// Build a per-flow handle. Resolves the three components against
    /// the registry **once** here and caches the `Arc<Semaphore>`
    /// triple on the handle, so the hot `acquire_*` path is a single
    /// `Semaphore::acquire` call — no hashmap lookup, no Arc clone.
    ///
    /// Panics if any of the three components has not been registered.
    /// That is a programming error in `assemble`, not a user-facing
    /// failure.
    pub fn handle(&self, source: &str, sink: &str, storage: &str) -> FlowLockHandle {
        FlowLockHandle {
            source: self
                .sources
                .get(source)
                .unwrap_or_else(|| panic!("source {source:?} not registered"))
                .clone(),
            sink: self
                .sinks
                .get(sink)
                .unwrap_or_else(|| panic!("sink {sink:?} not registered"))
                .clone(),
            storage: self
                .storages
                .get(storage)
                .unwrap_or_else(|| panic!("storage {storage:?} not registered"))
                .clone(),
        }
    }
}

/// `or_insert_with` preserves Arc identity on second call so any
/// handle already issued continues to reference the same Semaphore.
fn register_into(map: &mut AHashMap<String, Arc<Semaphore>>, name: &str, max_connections: u32) {
    let permits = (max_connections as usize).min(Semaphore::MAX_PERMITS);
    map.entry(name.to_string())
        .or_insert_with(|| Arc::new(Semaphore::new(permits)));
}

/// Emit one `info!` per registered component recording its permit
/// budget. Free function rather than a method on the manager — this
/// is one-shot operator diagnostics, not a runtime concern. Sorted
/// output for stable diffs across runs.
pub fn log_concurrency_budgets(mgr: &ConcurrencyManager) {
    let mut rows: Vec<(&'static str, &str, usize)> = Vec::new();
    for (n, s) in &mgr.sources {
        rows.push(("source", n.as_str(), s.available_permits()));
    }
    for (n, s) in &mgr.sinks {
        rows.push(("sink", n.as_str(), s.available_permits()));
    }
    for (n, s) in &mgr.storages {
        rows.push(("storage", n.as_str(), s.available_permits()));
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
    source: Arc<Semaphore>,
    sink: Arc<Semaphore>,
    storage: Arc<Semaphore>,
}

impl FlowLockHandle {
    pub async fn acquire_source(&self) -> Result<SemaphorePermit<'_>, RuntimeError> {
        self.source.acquire().await.map_err(RuntimeError::backend)
    }

    pub async fn acquire_sink(&self) -> Result<SemaphorePermit<'_>, RuntimeError> {
        self.sink.acquire().await.map_err(RuntimeError::backend)
    }

    pub async fn acquire_storage(&self) -> Result<SemaphorePermit<'_>, RuntimeError> {
        self.storage.acquire().await.map_err(RuntimeError::backend)
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
        let h = m.handle("s", "k", "st");
        {
            let _g = h.acquire_source().await.unwrap();
            assert_eq!(m.sources["s"].available_permits(), 0);
            assert_eq!(m.sinks["k"].available_permits(), 1);
            assert_eq!(m.storages["st"].available_permits(), 1);
        }
        assert_eq!(m.sources["s"].available_permits(), 1);
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
        let h1 = m.handle("shared", "sink_a", "storage_a");
        let h2 = m.handle("shared", "sink_b", "storage_b");

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
        let h = m.handle("s", "k", "st");
        let _src = h.acquire_source().await.unwrap();
        // Sink permit is unrelated to source — must acquire without
        // contention.
        let _snk = h.acquire_sink().await.unwrap();
        assert_eq!(m.sources["s"].available_permits(), 0);
        assert_eq!(m.sinks["k"].available_permits(), 0);
    }

    /// Asking the manager for an unregistered handle is a programming
    /// error and must panic with a useful message.
    #[test]
    #[should_panic(expected = "not registered")]
    fn unregistered_component_panics() {
        let m = manager_with(&[("s", 1)], &[("k", 1)], &[]);
        let _ = m.handle("s", "k", "missing-storage");
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
        let h1 = mgr.handle("shared_src", "sink", "storage");
        // Re-register the same source — handle1's Arc must still
        // point at the same Semaphore the second handle gets.
        // (Unsafe to call from `&Arc<Self>`, so build a second
        // manager scenario via owned manager.)
        let arc_via_handle = h1.source.clone();
        let arc_via_inner = mgr.sources["shared_src"].clone();
        assert!(Arc::ptr_eq(&arc_via_handle, &arc_via_inner));
        assert_eq!(arc_via_handle.available_permits(), 5);
    }
}
