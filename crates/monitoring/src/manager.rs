use std::sync::Arc;

use ahash::AHashMap;
use prometheus::core::{Collector, Desc};
use prometheus::proto::MetricFamily;
use prometheus::{IntCounterVec, IntGaugeVec, Opts, Registry};

use crate::config::PrometheusConfig;
use crate::error::MonitoringError;
use crate::integrating_gauge::TimeIntegratingGauge;
use crate::recorders::counts_collector::CountsCollector;
use crate::recorders::flow_recorder::{FlowLabels, FlowRecorder, FlowRecorderInner};
use crate::recorders::lock_recorder::{ComponentKind, LockRecorder, LockRecorderInner};
use crate::recorders::pool_stats_collector::{PoolStatsCollector, PoolStatsReader};
use crate::recorders::process_collector::ProcessCollector;
use crate::summary::Summary;

/// Construction-time handle for the Prometheus subsystem. Owns the
/// registry, every instrument vec, and a small idempotency cache so
/// repeated `flow_recorder(...)` / `lock_recorder(...)` calls with the
/// same labels return the same handle. Single-threaded by contract —
/// the cache lives behind `&mut self` so no `Mutex` is required.
///
/// After all recorders are minted (in assemble + flow engine setup),
/// the caller hands the manager off to [`MonitoringManager::into_scraper`]
/// which freezes it into a cheap [`MetricsScraper`] for the HTTP server
/// task.
pub struct MonitoringManager {
    inner: Option<ManagerInner>,
}

struct ManagerInner {
    config: PrometheusConfig,
    registry: Registry,
    fetch_summary: Summary,
    transform_summary: Summary,
    sink_summary: Summary,
    rows_total: IntCounterVec,
    errors: IntCounterVec,
    lock_max: IntGaugeVec,
    lock_queue: TimeIntegratingGauge,
    lock_active: TimeIntegratingGauge,
    counts: CountsCollector,
    pool_stats: PoolStatsCollector,
    flow_cache: AHashMap<FlowLabels, FlowRecorder>,
    lock_cache: AHashMap<(ComponentKind, String), LockRecorder>,
}

impl MonitoringManager {
    pub fn new(cfg: PrometheusConfig) -> Result<Self, MonitoringError> {
        cfg.validate()?;
        if !cfg.enabled {
            return Ok(MonitoringManager { inner: None });
        }
        Ok(MonitoringManager {
            inner: Some(ManagerInner::build(cfg)?),
        })
    }

    pub fn disabled() -> Self {
        MonitoringManager { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn config(&self) -> Option<&PrometheusConfig> {
        self.inner.as_ref().map(|i| &i.config)
    }

    /// Idempotent: calling with the same `labels` returns the same
    /// `FlowRecorder` (no fresh slot allocation, no fresh handle).
    pub fn flow_recorder(&mut self, labels: FlowLabels) -> FlowRecorder {
        let Some(inner) = self.inner.as_mut() else {
            return FlowRecorder::disabled();
        };
        if let Some(existing) = inner.flow_cache.get(&labels) {
            return existing.clone();
        }

        let fetch_slot = inner.fetch_summary.allocate(vec![
            labels.flow.clone(),
            labels.source_name.clone(),
            labels.source_kind.clone(),
        ]);
        let transform_slot = inner.transform_summary.allocate(vec![labels.flow.clone()]);
        let sink_slot = inner.sink_summary.allocate(vec![
            labels.flow.clone(),
            labels.sink_name.clone(),
            labels.sink_kind.clone(),
        ]);

        // Pre-extract 6 child counters of the unified `air_elt_rows_total`
        // family: 3 stages × 2 ops. Read stage uses the source labels;
        // written / skipped use the sink labels. Caching here means the
        // hot-path `inc_rows_*` is a single atomic add — no per-call
        // AHashMap lookup through `with_label_values`.
        let rows_read_upsert = inner
            .rows_total
            .with_label_values(&[
                &labels.flow,
                "read",
                &labels.source_name,
                &labels.source_kind,
                "upsert",
            ])
            .clone();
        let rows_read_delete = inner
            .rows_total
            .with_label_values(&[
                &labels.flow,
                "read",
                &labels.source_name,
                &labels.source_kind,
                "delete",
            ])
            .clone();
        let rows_written_upsert = inner
            .rows_total
            .with_label_values(&[
                &labels.flow,
                "written",
                &labels.sink_name,
                &labels.sink_kind,
                "upsert",
            ])
            .clone();
        let rows_written_delete = inner
            .rows_total
            .with_label_values(&[
                &labels.flow,
                "written",
                &labels.sink_name,
                &labels.sink_kind,
                "delete",
            ])
            .clone();
        let rows_skipped_upsert = inner
            .rows_total
            .with_label_values(&[
                &labels.flow,
                "skipped",
                &labels.sink_name,
                &labels.sink_kind,
                "upsert",
            ])
            .clone();
        let rows_skipped_delete = inner
            .rows_total
            .with_label_values(&[
                &labels.flow,
                "skipped",
                &labels.sink_name,
                &labels.sink_kind,
                "delete",
            ])
            .clone();

        let recorder = FlowRecorder::enabled(FlowRecorderInner {
            labels: labels.clone(),
            fetch_slot,
            transform_slot,
            sink_slot,
            rows_read_upsert,
            rows_read_delete,
            rows_written_upsert,
            rows_written_delete,
            rows_skipped_upsert,
            rows_skipped_delete,
            errors: inner.errors.clone(),
        });
        inner.flow_cache.insert(labels, recorder.clone());
        recorder
    }

    /// Idempotent: same `(kind, name)` returns the same `LockRecorder`.
    pub fn lock_recorder(&mut self, kind: ComponentKind, name: &str) -> LockRecorder {
        let Some(inner) = self.inner.as_mut() else {
            return LockRecorder::disabled();
        };
        let key = (kind, name.to_string());
        if let Some(existing) = inner.lock_cache.get(&key) {
            return existing.clone();
        }
        let lock_queue = inner
            .lock_queue
            .allocate(vec![kind.as_label().to_string(), name.to_string()]);
        let lock_active = inner
            .lock_active
            .allocate(vec![kind.as_label().to_string(), name.to_string()]);
        let recorder = LockRecorder::enabled(LockRecorderInner {
            lock_queue,
            lock_active,
        });
        inner.lock_cache.insert(key, recorder.clone());
        recorder
    }

    /// Set the configured lock max (semaphore permit count) for one
    /// component. Pure configuration — no concurrency state touched.
    pub fn set_lock_max(&mut self, kind: ComponentKind, name: &str, max: u32) {
        if let Some(inner) = self.inner.as_mut() {
            inner
                .lock_max
                .with_label_values(&[kind.as_label(), name])
                .set(i64::from(max));
        }
    }

    /// Register a connector's driver pool with monitoring in one shot.
    /// Publishes `air_elt_pool_connections_{max,min}` for the
    /// `(kind, name)` pair and wires the
    /// [`PoolStatsReader`] the collector polls on every scrape to
    /// surface live `active` / `idle` counts. Disabled manager: no-op.
    ///
    /// The reader is held strong by the collector for the process
    /// lifetime, so factories do not need to keep it alive themselves.
    /// Idempotent on `(kind, name)`: a repeat call refreshes the
    /// bounds, points the cached recorder at the new reader, and
    /// reuses the same active/idle gauge children.
    pub fn register_pool_stats(
        &mut self,
        kind: ComponentKind,
        name: &str,
        max: u32,
        min: u32,
        reader: Arc<dyn PoolStatsReader>,
    ) {
        let Some(inner) = self.inner.as_mut() else {
            return;
        };
        inner
            .pool_stats
            .register_pool_stats(kind, name, max, min, reader);
    }

    pub fn set_counts(&mut self, flows: u32, sources: u32, sinks: u32, storages: u32) {
        if let Some(inner) = self.inner.as_mut() {
            inner.counts.set(flows, sources, sinks, storages);
        }
    }

    /// Visible for tests — production code goes through
    /// [`MonitoringManager::into_scraper`] then scrapes via the scraper.
    pub fn gather(&self) -> Vec<prometheus::proto::MetricFamily> {
        match &self.inner {
            None => Vec::new(),
            Some(inner) => inner.registry.gather(),
        }
    }

    /// Freeze the manager into a shareable scrape-only handle for the
    /// HTTP server task. Consumes the construction-time caches.
    pub fn into_scraper(self) -> MetricsScraper {
        match self.inner {
            None => MetricsScraper::disabled(),
            Some(inner) => MetricsScraper {
                inner: Some(Arc::new(ScraperInner {
                    config: inner.config,
                    registry: inner.registry,
                })),
            },
        }
    }
}

impl ManagerInner {
    fn build(cfg: PrometheusConfig) -> Result<Self, MonitoringError> {
        let registry = Registry::new();
        let window = cfg.summary.window;
        let granularity = cfg.summary.bucket_granularity;
        let quantiles = cfg.summary.quantiles.clone();

        let fetch_summary = Summary::new(
            "air_elt_fetch_seconds",
            "Duration of the fetch (source.read_batch) phase",
            vec!["flow", "source", "source_kind"],
            window,
            granularity,
            quantiles.clone(),
        )?;
        let transform_summary = Summary::new(
            "air_elt_transform_seconds",
            "Duration of the transform phase",
            vec!["flow"],
            window,
            granularity,
            quantiles.clone(),
        )?;
        let sink_summary = Summary::new(
            "air_elt_sink_seconds",
            "Duration of the sink (sink.write_batch) phase",
            vec!["flow", "sink", "sink_kind"],
            window,
            granularity,
            quantiles,
        )?;
        registry.register(Box::new(fetch_summary.clone()))?;
        registry.register(Box::new(transform_summary.clone()))?;
        registry.register(Box::new(sink_summary.clone()))?;

        // Single rows family. `stage` ∈ {"read", "written", "skipped"};
        // `component` is the source for read, the sink for written /
        // skipped. Folded into one family so operators write
        // `sum by (stage) rate(air_elt_rows_total[5m])` once instead
        // of stitching three family names together.
        let rows_total = IntCounterVec::new(
            Opts::new(
                "air_elt_rows_total",
                "Per-flow row throughput, split by stage (read/written/skipped) and op",
            ),
            &["flow", "stage", "component", "component_kind", "op"],
        )?;
        let errors = IntCounterVec::new(
            Opts::new("air_elt_errors_total", "Total flow iteration errors"),
            &["flow", "stage", "stage_kind", "stage_name", "kind"],
        )?;
        // Wrap `rows_total` so pre-extracted children with value 0
        // stay out of `/metrics` (skip-zero policy). The recorder holds
        // its 6 child handles for the hot path; this wrapper only
        // gates emission, the inner Vec is otherwise untouched.
        // `errors_total` doesn't need the wrapper: its children are
        // materialised lazily via `with_label_values` only on the
        // first real error, so prometheus already won't emit them
        // until something fires.
        registry.register(Box::new(SkipZeroIntCounterVec::new(rows_total.clone())))?;
        registry.register(Box::new(errors.clone()))?;

        let lock_max = IntGaugeVec::new(
            Opts::new(
                "air_elt_lock_max",
                "Configured component lock size (semaphore permits)",
            ),
            &["kind", "component"],
        )?;
        registry.register(Box::new(lock_max.clone()))?;

        let lock_queue = TimeIntegratingGauge::new(
            "air_elt_lock_queue_seconds_integral",
            "Time-integral of callers waiting on the component lock",
            vec!["kind", "component"],
        )?;
        registry.register(Box::new(lock_queue.clone()))?;

        let lock_active = TimeIntegratingGauge::new(
            "air_elt_lock_active_seconds_integral",
            "Time-integral of held component lock permits",
            vec!["kind", "component"],
        )?;
        registry.register(Box::new(lock_active.clone()))?;

        let counts = CountsCollector::new(&registry)?;
        let process = ProcessCollector::new()?;
        registry.register(Box::new(process))?;

        let pool_stats = PoolStatsCollector::new()?;
        // The collector owns four `IntGaugeVec` families
        // (active/idle/max/min). It registers as a single Collector and
        // walks all four on every scrape.
        pool_stats.register(&registry)?;

        Ok(Self {
            config: cfg,
            registry,
            fetch_summary,
            transform_summary,
            sink_summary,
            rows_total,
            errors,
            lock_max,
            lock_queue,
            lock_active,
            counts,
            pool_stats,
            flow_cache: AHashMap::new(),
            lock_cache: AHashMap::new(),
        })
    }
}

/// Scrape-only handle. Cloneable, `Send`, used by the HTTP server task.
/// Carries the registry (which is internally `Arc`-shared) and the
/// config (for prefix / port readout).
#[derive(Clone, Default)]
pub struct MetricsScraper {
    inner: Option<Arc<ScraperInner>>,
}

struct ScraperInner {
    config: PrometheusConfig,
    registry: Registry,
}

impl MetricsScraper {
    pub fn disabled() -> Self {
        MetricsScraper { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn config(&self) -> Option<&PrometheusConfig> {
        self.inner.as_ref().map(|i| &i.config)
    }

    pub fn gather(&self) -> Vec<prometheus::proto::MetricFamily> {
        match &self.inner {
            None => Vec::new(),
            Some(inner) => inner.registry.gather(),
        }
    }
}

/// Wraps an `IntCounterVec` and filters out children whose value is
/// still zero at scrape time. The inner Vec is held by the recorder
/// for its 6 pre-extracted child counters (the hot path is
/// `IntCounter::inc_by` on those handles); this wrapper only gates the
/// emission side.
///
/// Without it, every child materialised at construction (the 6
/// `rows_total` slots in `FlowRecorder`) would appear in `/metrics`
/// with value 0 even when the flow has never fired that operation —
/// polluting `rate()` queries and dashboards. Filtering at the
/// emission boundary keeps the hot path branchless and the exposition
/// clean.
struct SkipZeroIntCounterVec {
    inner: IntCounterVec,
}

impl SkipZeroIntCounterVec {
    fn new(inner: IntCounterVec) -> Self {
        Self { inner }
    }
}

impl Collector for SkipZeroIntCounterVec {
    fn desc(&self) -> Vec<&Desc> {
        self.inner.desc()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let mut families = self.inner.collect();
        for family in families.iter_mut() {
            let metrics = std::mem::take(family.mut_metric());
            let filtered: Vec<_> = metrics
                .into_iter()
                .filter(|m| m.get_counter().get_value() > 0.0)
                .collect();
            *family.mut_metric() = filtered;
        }
        families
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::recorders::flow_recorder::{ErrorStage, RowOp};
    use crate::recorders::pool_stats_collector::PoolConnectionCounts;

    /// Tiny `PoolStatsReader` for the manager-level smoke tests. The
    /// per-backend wrappers live in `commons-*` crates we can't reach
    /// from here; this stand-in lets the test prove the manager
    /// forwards `register_pool_stats` into the collector cache.
    struct FakeReader {
        active: AtomicU32,
        idle: AtomicU32,
    }

    impl FakeReader {
        fn new(active: u32, idle: u32) -> Arc<Self> {
            Arc::new(Self {
                active: AtomicU32::new(active),
                idle: AtomicU32::new(idle),
            })
        }
    }

    impl crate::recorders::PoolStatsReader for FakeReader {
        fn read(&self) -> PoolConnectionCounts {
            PoolConnectionCounts {
                active: self.active.load(Ordering::Relaxed),
                idle: self.idle.load(Ordering::Relaxed),
            }
        }
    }

    fn enabled_cfg() -> PrometheusConfig {
        PrometheusConfig {
            enabled: true,
            ..PrometheusConfig::default()
        }
    }

    fn test_labels() -> FlowLabels {
        FlowLabels {
            flow: "orders".to_string(),
            source_name: "pg_src".to_string(),
            source_kind: "postgres".to_string(),
            sink_name: "ch_sink".to_string(),
            sink_kind: "clickhouse".to_string(),
            storage_name: "pg_state".to_string(),
            storage_kind: "postgres".to_string(),
        }
    }

    #[test]
    fn disabled_manager_returns_disabled_recorders() {
        let mut m = MonitoringManager::new(PrometheusConfig::default()).unwrap();
        assert!(!m.is_enabled());
        let flow = m.flow_recorder(test_labels());
        assert!(!flow.is_enabled());
        let pool = m.lock_recorder(ComponentKind::Source, "x");
        assert!(!pool.is_enabled());
        flow.inc_rows_read(100, RowOp::Upsert);
        flow.inc_error(ErrorStage::Sink, "backend");
        let _g = pool.active_guard();
        m.set_lock_max(ComponentKind::Source, "x", 5);
        // register_pool_stats on a disabled manager is a no-op — the
        // reader is dropped on return without ever being polled.
        m.register_pool_stats(
            ComponentKind::Source,
            "x",
            5,
            0,
            FakeReader::new(0, 0) as Arc<dyn crate::recorders::PoolStatsReader>,
        );
        assert!(m.gather().is_empty());
    }

    #[test]
    fn enabled_manager_registers_all_collectors() {
        let mut m = MonitoringManager::new(enabled_cfg()).unwrap();
        assert!(m.is_enabled());
        let flow = m.flow_recorder(test_labels());
        flow.inc_rows_read(7, RowOp::Upsert);
        flow.inc_rows_written(3, RowOp::Delete);
        flow.inc_rows_skipped(2, RowOp::Delete);
        flow.inc_error(ErrorStage::Fetch, "timeout");
        drop(flow.start_recording_fetch());
        drop(flow.start_recording_transform());
        drop(flow.start_recording_sink());
        let pool = m.lock_recorder(ComponentKind::Sink, "pg_sink");
        m.set_lock_max(ComponentKind::Sink, "pg_sink", 8);
        let _qg = pool.queue_guard();
        let _g = pool.active_guard();
        m.register_pool_stats(
            ComponentKind::Sink,
            "pg_sink",
            5,
            0,
            FakeReader::new(1, 1) as Arc<dyn crate::recorders::PoolStatsReader>,
        );
        m.set_counts(2, 1, 1, 1);

        let families = m.gather();
        let names: Vec<_> = families.iter().map(|f| f.name().to_string()).collect();
        for expected in [
            "air_elt_fetch_seconds",
            "air_elt_fetch_seconds_global",
            "air_elt_transform_seconds",
            "air_elt_transform_seconds_global",
            "air_elt_sink_seconds",
            "air_elt_sink_seconds_global",
            "air_elt_rows_total",
            "air_elt_errors_total",
            "air_elt_lock_max",
            "air_elt_lock_queue_seconds_integral",
            "air_elt_lock_active_seconds_integral",
            "air_elt_pool_connections_max",
            "air_elt_pool_connections_min",
            "flows",
            "sources",
            "sinks",
            "storages",
            "process_cpu_seconds_total",
            "process_resident_memory_bytes",
            "process_start_time_seconds",
            "memory_used_bytes_seconds_integral",
            "memory_available_bytes_seconds_integral",
            "memory_free_bytes_seconds_integral",
            "memory_total_bytes",
            "cpu_count",
        ] {
            assert!(names.iter().any(|n| n == expected), "missing {expected}");
        }
    }

    #[test]
    fn flow_recorder_is_idempotent() {
        let mut m = MonitoringManager::new(enabled_cfg()).unwrap();
        let a = m.flow_recorder(test_labels());
        let b = m.flow_recorder(test_labels());
        a.inc_rows_read(5, RowOp::Upsert);
        b.inc_rows_read(3, RowOp::Upsert);
        // Drive the fetch summary slot once via each recorder so the
        // skip-zero policy doesn't hide it; the assertion below
        // verifies that the two recorders feed the same single slot.
        drop(a.start_recording_fetch());
        drop(b.start_recording_fetch());
        let families = m.gather();
        let rows = families
            .iter()
            .find(|f| f.name() == "air_elt_rows_total")
            .unwrap();
        // Exactly one labelled child for the (flow, stage=read,
        // component=source, component_kind=source_kind, op=upsert)
        // tuple — proves both recorders point at the same counter,
        // not two counters that happen to sum to the same total.
        let upsert_read_children: Vec<_> = rows
            .get_metric()
            .iter()
            .filter(|metric| {
                let labels = metric.get_label();
                labels
                    .iter()
                    .any(|lp| lp.name() == "op" && lp.value() == "upsert")
                    && labels
                        .iter()
                        .any(|lp| lp.name() == "stage" && lp.value() == "read")
            })
            .collect();
        assert_eq!(
            upsert_read_children.len(),
            1,
            "expected one upsert/read child"
        );
        assert_eq!(
            upsert_read_children[0].get_counter().get_value() as u64,
            8,
            "expected 5 + 3 on the shared counter"
        );
        // Summary slot identity: the fetch summary's main family must
        // expose exactly one slot for the shared labels even though
        // two recorders were minted for the same `FlowLabels`.
        let fetch = families
            .iter()
            .find(|f| f.name() == "air_elt_fetch_seconds")
            .unwrap();
        assert_eq!(
            fetch.get_metric().len(),
            1,
            "two recorders share one summary slot"
        );
    }

    #[test]
    fn lock_recorder_is_idempotent() {
        let mut m = MonitoringManager::new(enabled_cfg()).unwrap();
        let a = m.lock_recorder(ComponentKind::Sink, "ch");
        let b = m.lock_recorder(ComponentKind::Sink, "ch");
        let _ag = a.active_guard();
        let _bg = b.active_guard();
        let families = m.gather();
        let active = families
            .iter()
            .find(|f| f.name() == "air_elt_lock_active_seconds_integral")
            .unwrap();
        // One slot per (kind, component) — second `lock_recorder` call
        // returns the cached recorder feeding the same slot.
        assert_eq!(
            active.get_metric().len(),
            1,
            "expected exactly one slot for idempotent recorder, got {}",
            active.get_metric().len()
        );
    }

    #[test]
    fn register_pool_stats_wires_through_manager() {
        // Smoke test: prove `MonitoringManager::register_pool_stats` plumbs
        // the bounds and the stats-reader into the same
        // `PoolStatsCollector` instance the scraper later gathers from.
        // Plain-gauge wiring is covered in
        // `pool_stats_collector::tests::collect_walks_attached_reader`
        // — duplicating it here adds nothing.
        let mut m = MonitoringManager::new(enabled_cfg()).unwrap();
        m.register_pool_stats(
            ComponentKind::Source,
            "pg_src",
            5,
            0,
            FakeReader::new(1, 0) as Arc<dyn crate::recorders::PoolStatsReader>,
        );

        let families = m.gather();
        let active = families
            .iter()
            .find(|f| f.name() == "air_elt_pool_connections_active")
            .expect("active gauge family present");
        let row = active
            .get_metric()
            .iter()
            .find(|metric| {
                let labels = metric.get_label();
                labels
                    .iter()
                    .any(|lp| lp.name() == "kind" && lp.value() == "source")
                    && labels
                        .iter()
                        .any(|lp| lp.name() == "component" && lp.value() == "pg_src")
            })
            .expect("expected (kind=source, component=pg_src) child");
        assert_eq!(row.get_gauge().get_value() as i64, 1);
    }

    #[test]
    fn skip_zero_counter_vec_hides_untouched_children() {
        // The 6 `rows_total` children are pre-extracted at `flow_recorder`
        // mint time so the hot path is a single `inc_by` without an
        // AHashMap lookup. Without the `SkipZeroIntCounterVec` wrapper,
        // all 6 would surface in `/metrics` carrying value 0 even when
        // the flow has only ever recorded one of them — polluting
        // `rate()` queries with phantom slices. The wrapper filters
        // any child whose value is still 0 at scrape time.
        let mut m = MonitoringManager::new(enabled_cfg()).unwrap();
        let flow = m.flow_recorder(test_labels());
        // Drive only one of the 6 children. The other 5 stay at 0.
        flow.inc_rows_read(5, RowOp::Upsert);

        let families = m.gather();
        let rows = families
            .iter()
            .find(|f| f.name() == "air_elt_rows_total")
            .expect("air_elt_rows_total family present");
        assert_eq!(
            rows.get_metric().len(),
            1,
            "skip-zero wrapper must filter the 5 untouched children, got {}",
            rows.get_metric().len()
        );
        let child = &rows.get_metric()[0];
        let labels = child.get_label();
        let flow_label = labels
            .iter()
            .find(|lp| lp.name() == "flow")
            .map(|lp| lp.value());
        let stage_label = labels
            .iter()
            .find(|lp| lp.name() == "stage")
            .map(|lp| lp.value());
        let op_label = labels
            .iter()
            .find(|lp| lp.name() == "op")
            .map(|lp| lp.value());
        assert_eq!(flow_label, Some("orders"));
        assert_eq!(stage_label, Some("read"));
        assert_eq!(op_label, Some("upsert"));
    }

    #[test]
    fn into_scraper_preserves_metrics() {
        let mut m = MonitoringManager::new(enabled_cfg()).unwrap();
        let flow = m.flow_recorder(test_labels());
        flow.inc_rows_read(11, RowOp::Upsert);
        let scraper = m.into_scraper();
        assert!(scraper.is_enabled());
        let families = scraper.gather();
        let rows = families
            .iter()
            .find(|f| f.name() == "air_elt_rows_total")
            .unwrap();
        let total: u64 = rows
            .get_metric()
            .iter()
            .filter(|metric| {
                metric
                    .get_label()
                    .iter()
                    .any(|lp| lp.name() == "stage" && lp.value() == "read")
            })
            .map(|m| m.get_counter().get_value() as u64)
            .sum();
        assert_eq!(total, 11);
    }
}
