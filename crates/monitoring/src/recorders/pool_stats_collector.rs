use std::sync::Arc;

use ahash::AHashMap;
use parking_lot::Mutex;
use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use prometheus::{IntGaugeVec, Opts, Registry};

use crate::error::MonitoringError;
use crate::recorders::lock_recorder::ComponentKind;

/// Live driver-pool counters. Returned by-value from
/// [`PoolStatsReader::read`]; the trait stays object-safe and the
/// caller never has to allocate.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolConnectionCounts {
    pub active: u32,
    pub idle: u32,
}

/// Stats reader for one connector's driver pool. Implementations
/// live in the per-backend `commons-*` crates so the monitoring crate
/// stays free of driver dependencies. `read()` is called once per
/// scrape per pool and must be cheap: a load from atomics for mongo,
/// an `O(1)` peek at sqlx internal counters for the sqlx-backed
/// connectors. Implementations are `Send + Sync` so `Arc<dyn
/// PoolStatsReader>` can be stored in [`PoolStatsCollector`]'s cache.
pub trait PoolStatsReader: Send + Sync {
    fn read(&self) -> PoolConnectionCounts;
}

/// Snapshot-driven recorder for a single connector's driver pool.
///
/// Internal plumbing — minted inside
/// [`PoolStatsCollector::register_pool_stats`] and held by the collector's
/// cache. The public surface is
/// [`crate::manager::MonitoringManager::register_pool_stats`], which
/// takes the configured bounds plus the `PoolStatsReader` and wires the
/// three pieces in one call. On every scrape,
/// [`PoolStatsCollector::collect`] reads the stats reader and calls
/// [`Self::set_state`] with the latest counts, which writes through to
/// the plain `active` / `idle` gauge children.
///
/// The recorder is the single write surface — sqlx is snapshot-at-
/// scrape from the driver pool; Mongo populates a `MongoPoolStatsReader`
/// atomic via CMAP events and is read snapshot-at-scrape too. The
/// on-method shape lives on `MongoPoolStatsReader`, not on this recorder.
///
/// `max`/`min` for the component are pinned by
/// [`PoolStatsCollector::register_pool_stats`] directly on the owning
/// `IntGaugeVec` — they are configuration values, not events, so they
/// do not need to live on the recorder.
///
/// No `disabled` variant: the recorder is only constructed by the
/// collector inside an enabled manager. A disabled
/// `MonitoringManager::register_pool_stats` returns early before ever
/// reaching the collector, so the no-op path is upstream and the
/// recorder itself can stay unconditionally enabled.
struct PoolStatsRecorder {
    /// Plain-gauge handles for the instant `active` / `idle` counts.
    /// Held alongside the parent `IntGaugeVec` so children materialise
    /// lazily on first `set_state` — skip-zero falls out naturally
    /// (prometheus only emits materialised children).
    active_gauge_vec: IntGaugeVec,
    idle_gauge_vec: IntGaugeVec,
    label_values: [String; 2],
}

impl PoolStatsRecorder {
    fn new(
        active_gauge_vec: IntGaugeVec,
        idle_gauge_vec: IntGaugeVec,
        kind: ComponentKind,
        name: &str,
    ) -> Self {
        Self {
            active_gauge_vec,
            idle_gauge_vec,
            label_values: [kind.as_label().to_string(), name.to_string()],
        }
    }

    /// Apply the latest driver-reported `(active, idle)` counts. Writes
    /// the plain-gauge children (instant truth at last scrape).
    /// Idempotent; the collector calls this from every scrape after
    /// reading the registered stats reader.
    ///
    /// Note: time-integrating gauges for active/idle were intentionally
    /// dropped — under snapshot-at-scrape they only integrate the last
    /// sampled value as a constant between scrapes, so they give no
    /// extra resolution over the plain gauge and lag by one scrape.
    /// If finer resolution is needed later, add an event-driven path
    /// (the mongo CMAP atomics already collect that information; sqlx
    /// would need its own).
    fn set_state(&self, counts: PoolConnectionCounts) {
        let labels = [self.label_values[0].as_str(), self.label_values[1].as_str()];
        self.active_gauge_vec
            .with_label_values(&labels)
            .set(i64::from(counts.active));
        self.idle_gauge_vec
            .with_label_values(&labels)
            .set(i64::from(counts.idle));
    }
}

/// One row in the stats-reader cache: the recorder that pushes counts
/// into the plain `active` / `idle` gauges, plus the stats reader the
/// collector polls on every scrape. `register_pool_stats` pairs them
/// at construction time; there is no "minted but unwired" state.
///
/// The reader is held by strong `Arc`. The collector lives for the
/// process lifetime and the app holds every reader's owning Arc for
/// the same span, so a `Weak` would just be ceremony — and forcing
/// connectors to also carry the strong Arc themselves to keep the
/// `Weak` upgradable leaked pool-stats plumbing into every connector
/// struct. With a strong Arc here that plumbing disappears entirely.
struct PoolEntry {
    recorder: PoolStatsRecorder,
    reader: Arc<dyn PoolStatsReader>,
}

/// Snapshot-driven collector for driver-pool metrics. Owns four
/// metric families and registers as a single
/// [`prometheus::core::Collector`]:
/// - `air_elt_pool_connections_active` (`IntGaugeVec`, plain)
/// - `air_elt_pool_connections_idle` (`IntGaugeVec`, plain)
/// - `air_elt_pool_connections_max` (`IntGaugeVec`, plain)
/// - `air_elt_pool_connections_min` (`IntGaugeVec`, plain)
///
/// On every scrape, [`Self::collect`] walks the stats-reader cache,
/// reads each registered `Arc<dyn PoolStatsReader>`, and pushes the
/// latest `(active, idle)` counts into the matching recorder before
/// delegating to each inner collector's own `collect`. Cheap under
/// the typical cardinality (≤ N connectors per flow × few flows).
#[derive(Clone)]
pub struct PoolStatsCollector {
    inner: Arc<Inner>,
}

struct Inner {
    active: IntGaugeVec,
    idle: IntGaugeVec,
    max: IntGaugeVec,
    min: IntGaugeVec,
    /// Idempotency cache so a repeat `register_recorder(kind, name, ...)`
    /// returns the same recorder, and so the scrape path can walk every
    /// live `(kind, name)` for stats-reader refresh.
    cache: Mutex<AHashMap<(ComponentKind, String), PoolEntry>>,
    /// Aggregate `Desc` list — built once so every scrape can return a
    /// borrowed view via [`Collector::desc`] without re-walking the
    /// inner collectors.
    descs: Vec<Desc>,
}

impl PoolStatsCollector {
    pub fn new() -> Result<Self, MonitoringError> {
        let active = IntGaugeVec::new(
            Opts::new(
                "air_elt_pool_connections_active",
                "Driver-pool active connections (instant value at last scrape)",
            ),
            &["kind", "component"],
        )?;
        let idle = IntGaugeVec::new(
            Opts::new(
                "air_elt_pool_connections_idle",
                "Driver-pool idle connections (instant value at last scrape)",
            ),
            &["kind", "component"],
        )?;
        let max = IntGaugeVec::new(
            Opts::new(
                "air_elt_pool_connections_max",
                "Driver-pool maximum capacity",
            ),
            &["kind", "component"],
        )?;
        let min = IntGaugeVec::new(
            Opts::new(
                "air_elt_pool_connections_min",
                "Driver-pool minimum idle floor",
            ),
            &["kind", "component"],
        )?;
        let mut descs = Vec::new();
        descs.extend(active.desc().into_iter().cloned());
        descs.extend(idle.desc().into_iter().cloned());
        descs.extend(max.desc().into_iter().cloned());
        descs.extend(min.desc().into_iter().cloned());
        Ok(Self {
            inner: Arc::new(Inner {
                active,
                idle,
                max,
                min,
                cache: Mutex::new(AHashMap::new()),
                descs,
            }),
        })
    }

    /// Register the collector against the shared prometheus registry.
    /// Called once at `MonitoringManager` build. The collector is the
    /// only thing registered — the four `IntGaugeVec`s it owns get
    /// walked from inside `Self::collect` rather than registered
    /// independently.
    pub fn register(&self, registry: &Registry) -> Result<(), MonitoringError> {
        registry.register(Box::new(self.clone()))?;
        Ok(())
    }

    /// Register a pool in one shot: pin the
    /// `air_elt_pool_connections_{max,min}` gauges, mint (or fetch the
    /// cached) [`PoolStatsRecorder`], and attach the
    /// [`PoolStatsReader`] the collector polls on every scrape.
    /// Idempotent on `(kind, name)`: a second call refreshes the
    /// stored bounds, returns the same recorder pointing at the
    /// **same** active/idle gauge children, and replaces the stored
    /// reader with the one passed in.
    pub(crate) fn register_pool_stats(
        &self,
        kind: ComponentKind,
        name: &str,
        max: u32,
        min: u32,
        reader: Arc<dyn PoolStatsReader>,
    ) {
        let mut cache = self.inner.cache.lock();
        let label_values = [kind.as_label(), name];
        self.inner
            .max
            .with_label_values(&label_values)
            .set(i64::from(max));
        self.inner
            .min
            .with_label_values(&label_values)
            .set(i64::from(min));
        let key = (kind, name.to_string());
        if let Some(entry) = cache.get_mut(&key) {
            entry.reader = reader;
            return;
        }
        let recorder = PoolStatsRecorder::new(
            self.inner.active.clone(),
            self.inner.idle.clone(),
            kind,
            name,
        );
        cache.insert(key, PoolEntry { recorder, reader });
    }

    /// Pull the latest counts from every registered stats reader and feed
    /// them into the matching recorder. The collector holds strong
    /// `Arc`s, so there is nothing to evict — entries live for the
    /// process lifetime, same as the connector pools they describe.
    fn refresh_snapshots(&self) {
        let cache = self.inner.cache.lock();
        for entry in cache.values() {
            entry.recorder.set_state(entry.reader.read());
        }
    }
}

impl Collector for PoolStatsCollector {
    fn desc(&self) -> Vec<&Desc> {
        self.inner.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        self.refresh_snapshots();
        let mut out = Vec::with_capacity(4);
        out.extend(self.inner.active.collect());
        out.extend(self.inner.idle.collect());
        out.extend(self.inner.max.collect());
        out.extend(self.inner.min.collect());
        out
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Minimal `PoolStatsReader` implementation backed by atomic counts;
    /// stand-in for the per-backend wrappers that live in
    /// `commons-*`. Tests drive it via the public mutators.
    use std::sync::atomic::{AtomicU32, Ordering};

    struct FakeReader {
        active: AtomicU32,
        idle: AtomicU32,
    }

    impl FakeReader {
        fn new() -> Self {
            Self {
                active: AtomicU32::new(0),
                idle: AtomicU32::new(0),
            }
        }

        fn set(&self, active: u32, idle: u32) {
            self.active.store(active, Ordering::Relaxed);
            self.idle.store(idle, Ordering::Relaxed);
        }
    }

    impl PoolStatsReader for FakeReader {
        fn read(&self) -> PoolConnectionCounts {
            PoolConnectionCounts {
                active: self.active.load(Ordering::Relaxed),
                idle: self.idle.load(Ordering::Relaxed),
            }
        }
    }

    #[test]
    fn register_pins_max_min() {
        let collector = PoolStatsCollector::new().unwrap();
        let reader = Arc::new(FakeReader::new());
        collector.register_pool_stats(
            ComponentKind::Source,
            "pg_src",
            10,
            2,
            reader as Arc<dyn PoolStatsReader>,
        );
        let max_fams = collector.inner.max.collect();
        let min_fams = collector.inner.min.collect();
        assert_eq!(max_fams[0].get_metric().len(), 1);
        assert_eq!(
            max_fams[0].get_metric()[0].get_gauge().get_value() as i64,
            10
        );
        assert_eq!(
            min_fams[0].get_metric()[0].get_gauge().get_value() as i64,
            2
        );
    }

    #[test]
    fn re_register_refreshes_bounds_and_reader() {
        let collector = PoolStatsCollector::new().unwrap();
        let reader_a = Arc::new(FakeReader::new());
        reader_a.set(1, 1);
        collector.register_pool_stats(
            ComponentKind::Sink,
            "snk",
            5,
            0,
            reader_a as Arc<dyn PoolStatsReader>,
        );
        let reader_b = Arc::new(FakeReader::new());
        reader_b.set(3, 0);
        collector.register_pool_stats(
            ComponentKind::Sink,
            "snk",
            20,
            4,
            reader_b.clone() as Arc<dyn PoolStatsReader>,
        );
        let max_fams = collector.inner.max.collect();
        assert_eq!(
            max_fams[0].get_metric().len(),
            1,
            "no duplicate (kind, component) entry on max"
        );
        assert_eq!(
            max_fams[0].get_metric()[0].get_gauge().get_value() as i64,
            20,
            "max refreshed"
        );
        // Only one cache entry survives — the second call must have
        // refreshed the existing entry in place.
        assert_eq!(collector.inner.cache.lock().len(), 1);

        // Scrape must read through the *new* reader.
        let _ = collector.collect();
        let active_now = collector
            .inner
            .active
            .with_label_values(&["sink", "snk"])
            .get();
        assert_eq!(active_now, 3, "scrape uses the replaced reader");
    }

    #[test]
    fn collect_walks_attached_reader() {
        let collector = PoolStatsCollector::new().unwrap();
        let reader = Arc::new(FakeReader::new());
        reader.set(2, 1);
        collector.register_pool_stats(
            ComponentKind::Source,
            "pg_src",
            5,
            0,
            reader.clone() as Arc<dyn PoolStatsReader>,
        );

        // First scrape pulls (2, 1) into the recorder.
        let _ = collector.collect();
        let active_now = collector
            .inner
            .active
            .with_label_values(&["source", "pg_src"])
            .get();
        let idle_now = collector
            .inner
            .idle
            .with_label_values(&["source", "pg_src"])
            .get();
        assert_eq!(active_now, 2);
        assert_eq!(idle_now, 1);

        // Mutate reader, scrape again — plain gauges follow the
        // driver in lockstep.
        reader.set(0, 3);
        let _ = collector.collect();
        let active_now = collector
            .inner
            .active
            .with_label_values(&["source", "pg_src"])
            .get();
        let idle_now = collector
            .inner
            .idle
            .with_label_values(&["source", "pg_src"])
            .get();
        assert_eq!(active_now, 0);
        assert_eq!(idle_now, 3);
    }
}
