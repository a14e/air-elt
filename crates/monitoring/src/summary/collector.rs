use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Mutex, RwLock};
use prometheus::core::{Collector, Desc};
use prometheus::proto::Summary as ProtoSummary;
use prometheus::proto::{LabelPair, Metric, MetricFamily, MetricType, Quantile};
use sketches_ddsketch::{Config, DDSketch};

use crate::summary::windowed_sketch::WindowedSketch;

/// Custom Summary metric. The Arc lives inside the wrapper — Clone is
/// cheap, the type is the registration handle, the recorder handle,
/// and the scrape collector all in one. Slot handles carry a direct
/// `Arc<Mutex<…>>` to their own state, so observers and scrapers
/// contend only on the single slot they touch.
#[derive(Clone)]
pub struct Summary {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    global_name: String,
    label_names: Vec<&'static str>,
    window: Duration,
    granularity: Duration,
    quantiles: Vec<f64>,
    descs: Vec<Desc>,
    /// Read on every scrape, written only during `allocate` (assemble
    /// phase). `RwLock` lets parallel scrapes proceed without blocking
    /// each other; the rare allocator takes the write side.
    slots: RwLock<Vec<SummarySlot>>,
}

/// Internal state of one slot. Cumulative `count`/`sum` follow the
/// Prometheus Summary contract (monotonic, never decrease on eviction);
/// the windowed sketch carries the quantile material.
pub(crate) struct SummarySlotInner {
    label_values: Vec<String>,
    sketch: WindowedSketch,
    count: u64,
    sum: f64,
}

impl SummarySlotInner {
    fn observe(&mut self, value: f64) {
        self.sketch.record(value);
        self.count += 1;
        self.sum += value;
    }
}

/// Smart handle around a per-slot mutex. Cloning bumps the inner Arc;
/// recorders, the parent `Summary`'s `slots` vec, and (during a scrape)
/// the temporary snapshot all share clones.
#[derive(Clone)]
pub struct SummarySlot {
    inner: Arc<Mutex<SummarySlotInner>>,
}

impl SummarySlot {
    fn new(label_values: Vec<String>, window: Duration, granularity: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SummarySlotInner {
                label_values,
                sketch: WindowedSketch::new(window, granularity),
                count: 0,
                sum: 0.0,
            })),
        }
    }

    pub fn observe(&self, value: f64) {
        self.inner.lock().observe(value);
    }
}

impl Summary {
    pub fn new(
        name: impl Into<String>,
        help: impl Into<String>,
        label_names: Vec<&'static str>,
        window: Duration,
        granularity: Duration,
        quantiles: Vec<f64>,
    ) -> prometheus::Result<Self> {
        let name = name.into();
        let help = help.into();
        let global_name = format!("{name}_global");

        let main_desc = Desc::new(
            name.clone(),
            help.clone(),
            label_names.iter().map(|s| (*s).to_string()).collect(),
            std::collections::HashMap::new(),
        )?;
        let global_desc = Desc::new(
            global_name.clone(),
            format!("{help} (aggregated across labels)"),
            Vec::new(),
            std::collections::HashMap::new(),
        )?;

        // `help` is consumed by the two `Desc::new` calls above; both
        // descs now own a copy. `collect()` reads the help text back
        // from `descs[0].help` / `descs[1].help` instead of duplicating
        // it on `Inner`.
        Ok(Self {
            inner: Arc::new(Inner {
                name,
                global_name,
                label_names,
                window,
                granularity,
                quantiles,
                descs: vec![main_desc, global_desc],
                slots: RwLock::new(Vec::new()),
            }),
        })
    }

    /// Allocate a new slot. Callers should hold the returned
    /// `SummarySlot` for the lifetime of the recorder; idempotency on
    /// repeated label tuples is the caller's responsibility.
    pub fn allocate(&self, label_values: Vec<String>) -> SummarySlot {
        debug_assert_eq!(
            label_values.len(),
            self.inner.label_names.len(),
            "summary {:?}: label arity mismatch on allocate",
            self.inner.name
        );
        let slot = SummarySlot::new(label_values, self.inner.window, self.inner.granularity);
        self.inner.slots.write().push(slot.clone());
        slot
    }
}

impl Collector for Summary {
    fn desc(&self) -> Vec<&Desc> {
        self.inner.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        // Briefly take the outer read lock to snapshot the slot list,
        // then drop it before touching per-slot mutexes. The `_global`
        // rollup is no longer point-in-time consistent across slots —
        // observations recorded while collect walks the snapshot land
        // in some slots' `merge_live` and not others. This is the same
        // loss of cross-family atomicity Prometheus has between metric
        // families on a single scrape; operators don't rely on it.
        let slots: Vec<SummarySlot> = self.inner.slots.read().clone();

        let mut main_family = MetricFamily::default();
        main_family.set_name(self.inner.name.clone());
        main_family.set_help(self.inner.descs[0].help.clone());
        main_family.set_field_type(MetricType::SUMMARY);

        let mut global_sketch = DDSketch::new(Config::defaults());
        let mut global_count: u64 = 0;
        let mut global_sum: f64 = 0.0;

        for slot in &slots {
            let mut guard = slot.inner.lock();
            if guard.count == 0 {
                // Skip-zero policy: slots that never recorded an
                // observation stay out of `/metrics`. They still
                // contribute zero to the `_global` rollup (free), but
                // they do not pollute the labelled family with empty
                // {quantile} / _count / _sum rows.
                continue;
            }
            let per_slot_sketch = guard.sketch.merge_live();
            global_sketch
                .merge(&per_slot_sketch)
                .expect("ring sketches share one Config");
            global_count += guard.count;
            global_sum += guard.sum;

            let mut metric = Metric::default();
            metric.set_label(build_label_pairs(
                &self.inner.label_names,
                &guard.label_values,
            ));
            metric.set_summary(build_proto_summary(
                &per_slot_sketch,
                guard.count,
                guard.sum,
                &self.inner.quantiles,
            ));
            drop(guard);
            main_family.mut_metric().push(metric);
        }

        // Skip-zero policy extends to the `_global` rollup: if no slot
        // observed anything, omit the rollup family entirely instead
        // of emitting an empty `{}` row with zero count/sum and NaN
        // quantiles. Per Q4 resolution.
        let mut families = vec![main_family];
        if global_count > 0 {
            let mut global_family = MetricFamily::default();
            global_family.set_name(self.inner.global_name.clone());
            global_family.set_help(self.inner.descs[1].help.clone());
            global_family.set_field_type(MetricType::SUMMARY);
            let mut global_metric = Metric::default();
            global_metric.set_summary(build_proto_summary(
                &global_sketch,
                global_count,
                global_sum,
                &self.inner.quantiles,
            ));
            global_family.mut_metric().push(global_metric);
            families.push(global_family);
        }
        families
    }
}

fn build_label_pairs(names: &[&'static str], values: &[String]) -> Vec<LabelPair> {
    names
        .iter()
        .zip(values.iter())
        .map(|(name, value)| {
            let mut lp = LabelPair::default();
            lp.set_name((*name).to_string());
            lp.set_value(value.clone());
            lp
        })
        .collect()
}

fn build_proto_summary(sketch: &DDSketch, count: u64, sum: f64, quantiles: &[f64]) -> ProtoSummary {
    let mut s = ProtoSummary::default();
    s.set_sample_count(count);
    s.set_sample_sum(sum);
    let qs: Vec<Quantile> = quantiles
        .iter()
        .map(|&q| {
            let mut quantile = Quantile::default();
            quantile.set_quantile(q);
            let value = match sketch.quantile(q) {
                Ok(Some(v)) => v,
                _ => f64::NAN,
            };
            quantile.set_value(value);
            quantile
        })
        .collect();
    s.set_quantile(qs);
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn allocates_isolated_slots_per_caller() {
        let s = Summary::new(
            "test_seconds",
            "test",
            vec!["flow"],
            Duration::from_secs(60),
            Duration::from_secs(1),
            vec![0.5],
        )
        .unwrap();
        let a = s.allocate(vec!["a".to_string()]);
        let b = s.allocate(vec!["b".to_string()]);
        for v in [1.0, 2.0, 3.0] {
            a.observe(v);
        }
        for v in [10.0, 20.0] {
            b.observe(v);
        }
        let families = s.collect();
        assert_eq!(families[0].get_metric().len(), 2);
        let global = &families[1];
        assert_eq!(global.get_metric()[0].get_summary().sample_count(), 5);
        assert!((global.get_metric()[0].get_summary().sample_sum() - 36.0).abs() < 1e-9);
    }

    /// Skip-zero policy: a slot that's allocated but never observed
    /// must NOT appear in `collect()`'s main family. A second
    /// allocated-and-observed slot must appear. Probes the
    /// `count == 0` gate directly.
    #[test]
    fn untouched_slot_is_omitted_while_observed_slot_is_emitted() {
        let s = Summary::new(
            "skipzero_seconds",
            "skipzero",
            vec!["flow"],
            Duration::from_secs(60),
            Duration::from_secs(1),
            vec![0.5],
        )
        .unwrap();
        let _untouched = s.allocate(vec!["a".to_string()]);
        let driven = s.allocate(vec!["b".to_string()]);
        driven.observe(1.0);

        let families = s.collect();
        assert_eq!(
            families[0].get_metric().len(),
            1,
            "skip-zero must omit the untouched slot"
        );
        let label = &families[0].get_metric()[0].get_label()[0];
        assert_eq!(
            label.value(),
            "b",
            "the emitted slot must be the driven one"
        );
    }

    #[test]
    fn empty_summary_skips_global_rollup() {
        // Per Q4 resolution: skip-zero applies to `_global` too. A
        // summary that never observed anything emits just the main
        // family with no children — no zero-count `_global` row.
        let s = Summary::new(
            "empty_seconds",
            "empty",
            vec!["flow"],
            Duration::from_secs(60),
            Duration::from_secs(1),
            vec![0.5],
        )
        .unwrap();
        let families = s.collect();
        assert_eq!(families.len(), 1, "no _global rollup when totally empty");
        assert_eq!(families[0].get_metric().len(), 0);
        assert_eq!(families[0].name(), "empty_seconds");
    }

    #[test]
    fn cumulative_count_survives_window_eviction() {
        let s = Summary::new(
            "cum_seconds",
            "cumulative",
            vec!["flow"],
            Duration::from_millis(80),
            Duration::from_millis(20),
            vec![0.5],
        )
        .unwrap();
        let slot = s.allocate(vec!["x".to_string()]);
        slot.observe(1.0);
        slot.observe(2.0);
        std::thread::sleep(Duration::from_millis(150));
        slot.observe(3.0);
        let families = s.collect();
        assert_eq!(families[0].get_metric()[0].get_summary().sample_count(), 3);
        assert!((families[0].get_metric()[0].get_summary().sample_sum() - 6.0).abs() < 1e-9);
    }

    #[test]
    fn registers_in_prometheus_registry() {
        use prometheus::Registry;
        let registry = Registry::new();
        let summary = Summary::new(
            "rg_test_seconds",
            "registry test",
            vec!["flow"],
            Duration::from_secs(60),
            Duration::from_secs(1),
            vec![0.5],
        )
        .unwrap();
        let slot = summary.allocate(vec!["x".to_string()]);
        registry.register(Box::new(summary.clone())).unwrap();
        slot.observe(1.0);
        let families = registry.gather();
        let names: Vec<_> = families.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"rg_test_seconds"));
        assert!(names.contains(&"rg_test_seconds_global"));
    }

    /// Drives two observer threads against two different slots while a
    /// third thread scrapes via `collect()`. The point isn't to assert
    /// throughput numbers — it's to prove the per-slot locking lets
    /// observers and a scraper coexist without deadlock or lock-order
    /// inversion. If a future refactor ever folds the per-slot mutex
    /// back into a shared one, this test will still pass but the
    /// underlying contention story changes; the comment is the canary.
    #[test]
    fn observers_and_collector_can_run_concurrently() {
        let s = Summary::new(
            "stress_seconds",
            "stress",
            vec!["flow"],
            Duration::from_secs(60),
            Duration::from_secs(1),
            vec![0.5],
        )
        .unwrap();
        let a = s.allocate(vec!["a".to_string()]);
        let b = s.allocate(vec!["b".to_string()]);

        std::thread::scope(|scope| {
            let s_for_collector = s.clone();
            scope.spawn(move || {
                for _ in 0..1_000 {
                    a.observe(1.0);
                }
            });
            scope.spawn(move || {
                for _ in 0..1_000 {
                    b.observe(2.0);
                }
            });
            scope.spawn(move || {
                for _ in 0..200 {
                    let _ = s_for_collector.collect();
                }
            });
        });

        let families = s.collect();
        let total = families[1].get_metric()[0].get_summary().sample_count();
        assert_eq!(total, 2_000, "global rollup should observe every sample");
    }
}
