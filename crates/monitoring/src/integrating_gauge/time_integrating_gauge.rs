use std::sync::Arc;
use std::time::Instant;

use parking_lot::{Mutex, RwLock};
use prometheus::core::{Collector, Desc};
use prometheus::proto::{Counter, LabelPair, Metric, MetricFamily, MetricType};

/// A gauge whose scraped value is the time-integral of an underlying
/// signal. On every `update` we Kahan-add `last_value * dt` to the
/// accumulator; on scrape we virtually-tick the accumulator up to the
/// scrape instant so `d(metric)/dt` reads out as the time-averaged
/// signal.
///
/// `Clone` is cheap (Arc bump). The Arc lives inside the gauge — the
/// type is the registration handle, the recorder handle, and the
/// scrape collector all in one. Slot handles carry a direct
/// `Arc<Mutex<…>>` to their own inner state, so `set`/`add` block only
/// on the slot they touch and never contend with each other or with a
/// scrape walking sibling slots.
#[derive(Clone)]
pub struct TimeIntegratingGauge {
    inner: Arc<Inner>,
}

struct Inner {
    name: String,
    help: String,
    label_names: Vec<&'static str>,
    descs: Vec<Desc>,
    /// Read on every scrape, written only during `allocate` (assemble
    /// phase). An `RwLock` lets parallel scrapes proceed without
    /// blocking each other; the rare allocator takes the write side.
    slots: RwLock<Vec<IntegratingGaugeSlot>>,
}

pub(crate) struct IntegratingGaugeSlotInner {
    label_values: Vec<String>,
    acc: f64,
    /// Kahan compensation term.
    kahan_c: f64,
    last_value: f64,
    last_at: Instant,
    /// `false` until the first `set` / `add` call. Drives the
    /// skip-zero emission policy: an allocated-but-never-driven slot
    /// (e.g. a pool that was created but never saw a connection)
    /// stays out of `/metrics` until something actually moves it.
    touched: bool,
}

impl IntegratingGaugeSlotInner {
    fn new(label_values: Vec<String>, now: Instant) -> Self {
        Self {
            label_values,
            acc: 0.0,
            kahan_c: 0.0,
            last_value: 0.0,
            last_at: now,
            touched: false,
        }
    }

    fn integrate(&mut self, now: Instant) {
        if now <= self.last_at {
            return;
        }
        let dt = now.duration_since(self.last_at).as_secs_f64();
        let y = self.last_value * dt - self.kahan_c;
        let t = self.acc + y;
        self.kahan_c = (t - self.acc) - y;
        self.acc = t;
        self.last_at = now;
    }

    fn set(&mut self, value: f64, now: Instant) {
        self.integrate(now);
        self.last_value = value;
        self.touched = true;
    }

    fn add(&mut self, delta: f64, now: Instant) {
        self.integrate(now);
        self.last_value += delta;
        self.touched = true;
    }

    fn snapshot(&mut self, now: Instant) -> f64 {
        self.integrate(now);
        self.acc
    }
}

/// Smart handle around a per-slot mutex. Cloning bumps the inner Arc
/// only — recorders keep a clone, the parent `TimeIntegratingGauge`
/// keeps a clone, and `collect()` walks the parent's vec of clones.
#[derive(Clone)]
pub struct IntegratingGaugeSlot {
    inner: Arc<Mutex<IntegratingGaugeSlotInner>>,
}

impl IntegratingGaugeSlot {
    fn new(label_values: Vec<String>, now: Instant) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IntegratingGaugeSlotInner::new(
                label_values,
                now,
            ))),
        }
    }

    pub fn set(&self, value: f64) {
        self.inner.lock().set(value, Instant::now());
    }

    pub fn add(&self, delta: f64) {
        self.inner.lock().add(delta, Instant::now());
    }
}

impl TimeIntegratingGauge {
    pub fn new(
        name: impl Into<String>,
        help: impl Into<String>,
        label_names: Vec<&'static str>,
    ) -> prometheus::Result<Self> {
        let name = name.into();
        let help = help.into();
        debug_assert!(
            name.ends_with("_seconds_integral"),
            "TimeIntegratingGauge name {name:?} must end with `_seconds_integral` — \
             the suffix is the wire contract that tells consumers to read this as \
             rate(metric[window]) for a time-averaged value"
        );
        let desc = Desc::new(
            name.clone(),
            help.clone(),
            label_names.iter().map(|s| (*s).to_string()).collect(),
            std::collections::HashMap::new(),
        )?;
        Ok(Self {
            inner: Arc::new(Inner {
                name,
                help,
                label_names,
                descs: vec![desc],
                slots: RwLock::new(Vec::new()),
            }),
        })
    }

    pub fn allocate(&self, label_values: Vec<String>) -> IntegratingGaugeSlot {
        debug_assert_eq!(
            label_values.len(),
            self.inner.label_names.len(),
            "integrating gauge {:?}: label arity mismatch on allocate",
            self.inner.name
        );
        let slot = IntegratingGaugeSlot::new(label_values, Instant::now());
        self.inner.slots.write().push(slot.clone());
        slot
    }

    /// Count of currently-allocated slots, regardless of whether each
    /// has been touched yet. Crate-internal; tests use this to assert
    /// allocation dedup independently of the skip-zero emission
    /// policy. Hot path uses neither lock — keep this off the scrape
    /// path.
    #[cfg(test)]
    pub(crate) fn allocated_slots(&self) -> usize {
        self.inner.slots.read().len()
    }
}

impl Collector for TimeIntegratingGauge {
    fn desc(&self) -> Vec<&Desc> {
        self.inner.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        // Briefly take the outer read lock to snapshot the slot list,
        // then drop it before touching per-slot mutexes. Allocations
        // (write side) only happen during assemble — so a scrape that
        // races with allocation is unmeasurably rare in practice, and
        // the read side never blocks parallel scrapes against each
        // other.
        let slots: Vec<IntegratingGaugeSlot> = self.inner.slots.read().clone();

        let mut family = MetricFamily::default();
        family.set_name(self.inner.name.clone());
        family.set_help(self.inner.help.clone());
        family.set_field_type(MetricType::COUNTER);
        for slot in &slots {
            // `now` is captured per slot rather than once for the
            // family — slot integrals diverge by microseconds across
            // the loop. The integrating math (`acc + last_value * dt`)
            // is per-slot anyway, so correctness holds and operators
            // never observe the divergence.
            let mut guard = slot.inner.lock();
            if !guard.touched {
                // Skip-zero policy: a slot allocated by a recorder but
                // never driven (no `set`/`add` fired) does not emit a
                // 0-valued row. Keeps `/metrics` quiet for connectors
                // that were minted but have no traffic yet.
                continue;
            }
            let value = guard.snapshot(Instant::now());
            let mut metric = Metric::default();
            metric.set_label(build_label_pairs(
                &self.inner.label_names,
                &guard.label_values,
            ));
            drop(guard);
            let mut counter = Counter::default();
            counter.set_value(value);
            metric.set_counter(counter);
            family.mut_metric().push(metric);
        }
        vec![family]
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn empty_gauge_emits_no_metrics() {
        let g = TimeIntegratingGauge::new("test_seconds_integral", "t", vec!["k"]).unwrap();
        let families = g.collect();
        assert_eq!(families[0].get_metric().len(), 0);
    }

    /// Skip-zero policy: a slot allocated but never driven (no
    /// `set`/`add` fired) must NOT appear in `collect()`. A second
    /// allocated-and-driven slot must appear. Probes the `touched`
    /// gate directly — without this test a regression that flips
    /// `if !guard.touched` to `if guard.touched` would still pass the
    /// other tests because they all drive their slots.
    #[test]
    fn untouched_slot_is_omitted_while_touched_slot_is_emitted() {
        let g = TimeIntegratingGauge::new("touched_seconds_integral", "t", vec!["k"]).unwrap();
        let _untouched = g.allocate(vec!["a".to_string()]);
        let driven = g.allocate(vec!["b".to_string()]);
        driven.set(1.0);

        // Both slots are allocated; only one is touched.
        assert_eq!(g.allocated_slots(), 2);
        let families = g.collect();
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

    /// Per-slot locking smoke test: drive two slots concurrently
    /// against a scraper. The point isn't throughput — it's that the
    /// per-slot mutex pattern lets observers and a scraper coexist
    /// without deadlock. A regression that re-folds the inner mutex
    /// back into a shared one would still pass this test but the
    /// contention story would change; this test plus the matching
    /// Summary test are the canaries.
    #[test]
    fn observers_and_collector_can_run_concurrently() {
        let g = TimeIntegratingGauge::new("stress_seconds_integral", "s", vec!["k"]).unwrap();
        let a = g.allocate(vec!["a".to_string()]);
        let b = g.allocate(vec!["b".to_string()]);

        std::thread::scope(|scope| {
            let g_for_collector = g.clone();
            scope.spawn(move || {
                for _ in 0..1_000 {
                    a.add(1.0);
                    a.add(-1.0);
                }
            });
            scope.spawn(move || {
                for _ in 0..1_000 {
                    b.set(1.0);
                    b.set(0.0);
                }
            });
            scope.spawn(move || {
                for _ in 0..200 {
                    let _ = g_for_collector.collect();
                }
            });
        });

        // Both slots survived the stress and emit (they were driven).
        let families = g.collect();
        assert_eq!(families[0].get_metric().len(), 2);
    }

    #[test]
    fn accumulates_over_time() {
        let g = TimeIntegratingGauge::new("acc_seconds_integral", "a", vec!["k"]).unwrap();
        let slot = g.allocate(vec!["x".to_string()]);
        slot.set(2.0);
        sleep(Duration::from_millis(150));
        let v1 = g.collect()[0].get_metric()[0].get_counter().get_value();
        assert!((v1 - 0.3).abs() < 0.05, "v1 = {v1}");
        sleep(Duration::from_millis(150));
        let v2 = g.collect()[0].get_metric()[0].get_counter().get_value();
        assert!(v2 > v1, "v2 ({v2}) must exceed v1 ({v1})");
    }

    #[test]
    fn add_and_set_compose() {
        let g = TimeIntegratingGauge::new("add_seconds_integral", "a", vec!["k"]).unwrap();
        let slot = g.allocate(vec!["x".to_string()]);
        slot.add(1.0);
        slot.add(1.0);
        sleep(Duration::from_millis(100));
        slot.add(-1.0);
        let v = g.collect()[0].get_metric()[0].get_counter().get_value();
        assert!((0.15..=0.35).contains(&v), "v = {v}");
    }

    #[test]
    fn snapshot_does_not_mutate_last_value() {
        let g = TimeIntegratingGauge::new("snap_seconds_integral", "s", vec!["k"]).unwrap();
        let slot = g.allocate(vec!["x".to_string()]);
        slot.set(5.0);
        sleep(Duration::from_millis(50));
        g.collect();
        sleep(Duration::from_millis(100));
        let v = g.collect()[0].get_metric()[0].get_counter().get_value();
        assert!(v > 0.4, "v = {v}");
    }

    #[test]
    fn monotonic_nondecreasing_for_nonneg_signal() {
        let g = TimeIntegratingGauge::new("mono_seconds_integral", "m", vec!["k"]).unwrap();
        let slot = g.allocate(vec!["x".to_string()]);
        slot.set(1.0);
        let mut prev = 0.0;
        for _ in 0..5 {
            sleep(Duration::from_millis(30));
            let v = g.collect()[0].get_metric()[0].get_counter().get_value();
            assert!(v >= prev, "non-monotonic: {v} < {prev}");
            prev = v;
        }
    }

    /// Drives ten thousand cancelling `add(±tiny)` pairs and asserts
    /// the accumulator stays near zero. Without Kahan compensation,
    /// repeated `acc + tiny*dt` would drift through double-precision
    /// rounding noise.
    #[test]
    fn kahan_compensation_keeps_acc_near_zero() {
        let g = TimeIntegratingGauge::new("kahan_seconds_integral", "k", vec!["k"]).unwrap();
        let slot = g.allocate(vec!["x".to_string()]);
        for _ in 0..10_000 {
            slot.add(1e-12);
            slot.add(-1e-12);
        }
        let v = g.collect()[0].get_metric()[0].get_counter().get_value();
        assert!(v.abs() < 1e-6, "accumulator drifted: {v}");
    }
}
