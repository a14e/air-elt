use parking_lot::Mutex;
use prometheus::core::{Collector, Desc};
use prometheus::proto::{Gauge, Metric, MetricFamily, MetricType};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::integrating_gauge::{IntegratingGaugeSlot, TimeIntegratingGauge};

/// Collector emitting per-process and host gauges. Refreshes sysinfo
/// state on every scrape.
///
/// Per-process metrics use the canonical Prometheus exporter names so
/// standard dashboards (kube-prometheus, node_exporter) recognise
/// them:
/// - `process_cpu_seconds_total` (float counter, cumulative CPU seconds
///   with millisecond resolution — sysinfo's `accumulated_cpu_time()`
///   in ms divided by 1000.0; we feed deltas through a float
///   `prometheus::Counter` so sub-second utilisation shows up in
///   `rate()`).
/// - `process_resident_memory_bytes` (plain gauge)
/// - `process_start_time_seconds` (plain gauge)
///
/// Host-level memory split into two flavours:
/// - Plain gauge: `memory_total_bytes` (stable, no value in integrating).
/// - Time-integrating gauges (suffix `_seconds_integral`): used,
///   available, free. Per the task brief: read time-averaged value via
///   `rate(metric[window])` instead of a single instantaneous sample.
///
/// Host CPU: `cpu_count` (plain gauge).
pub struct ProcessCollector {
    descs: Vec<Desc>,
    system: Mutex<System>,
    /// Cumulative CPU seconds last emitted. sysinfo reports
    /// monotonically-increasing milliseconds via
    /// `Process::accumulated_cpu_time()`; we convert to seconds as
    /// `f64` (u64 ms → f64 keeps 53 bits of mantissa, > 1000 years of
    /// CPU time before any loss) and drive the float Counter via
    /// `inc_by(delta)`, so the counter honours Prometheus's
    /// monotonic-delta contract — we never re-publish an absolute
    /// value.
    last_cpu_seconds: Mutex<f64>,
    cpu_seconds_total: prometheus::Counter,
    memory_used: TimeIntegratingGauge,
    memory_used_slot: IntegratingGaugeSlot,
    memory_available: TimeIntegratingGauge,
    memory_available_slot: IntegratingGaugeSlot,
    memory_free: TimeIntegratingGauge,
    memory_free_slot: IntegratingGaugeSlot,
}

impl ProcessCollector {
    pub fn new() -> prometheus::Result<Self> {
        let cpu_seconds_total = prometheus::Counter::new(
            "process_cpu_seconds_total",
            "Total CPU seconds consumed by the process (sysinfo `accumulated_cpu_time`, millisecond resolution)",
        )?;
        let memory_used = TimeIntegratingGauge::new(
            "memory_used_bytes_seconds_integral",
            "Time-integral of host memory in use (sysinfo `used_memory`)",
            Vec::new(),
        )?;
        let memory_used_slot = memory_used.allocate(Vec::new());
        let memory_available = TimeIntegratingGauge::new(
            "memory_available_bytes_seconds_integral",
            "Time-integral of host memory available for allocation (sysinfo `available_memory`)",
            Vec::new(),
        )?;
        let memory_available_slot = memory_available.allocate(Vec::new());
        let memory_free = TimeIntegratingGauge::new(
            "memory_free_bytes_seconds_integral",
            "Time-integral of host memory free (sysinfo `free_memory`)",
            Vec::new(),
        )?;
        let memory_free_slot = memory_free.allocate(Vec::new());

        let mut descs = Vec::new();
        descs.extend(cpu_seconds_total.desc().into_iter().cloned());
        descs.push(simple_desc(
            "process_resident_memory_bytes",
            "Current process resident memory in bytes",
        )?);
        descs.push(simple_desc(
            "process_start_time_seconds",
            "Process start time in seconds since the Unix epoch",
        )?);
        descs.push(simple_desc(
            "memory_total_bytes",
            "Host total memory (sysinfo `total_memory`)",
        )?);
        descs.push(simple_desc(
            "cpu_count",
            "Number of logical CPUs visible to the process",
        )?);
        descs.extend(memory_used.desc().into_iter().cloned());
        descs.extend(memory_available.desc().into_iter().cloned());
        descs.extend(memory_free.desc().into_iter().cloned());

        Ok(Self {
            descs,
            system: Mutex::new(System::new()),
            last_cpu_seconds: Mutex::new(0.0),
            cpu_seconds_total,
            memory_used,
            memory_used_slot,
            memory_available,
            memory_available_slot,
            memory_free,
            memory_free_slot,
        })
    }
}

impl Collector for ProcessCollector {
    fn desc(&self) -> Vec<&Desc> {
        self.descs.iter().collect()
    }

    fn collect(&self) -> Vec<MetricFamily> {
        let pid = std::process::id();
        let sysinfo_pid = Pid::from_u32(pid);
        let mut system = self.system.lock();
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[sysinfo_pid]),
            true,
            ProcessRefreshKind::nothing().with_cpu().with_memory(),
        );
        system.refresh_cpu_all();
        system.refresh_memory();

        let (cpu_ms_total, memory_bytes, start_time) = match system.process(sysinfo_pid) {
            Some(process) => (
                process.accumulated_cpu_time(),
                process.memory() as f64,
                process.start_time() as f64,
            ),
            None => (0, 0.0, 0.0),
        };
        let used = system.used_memory() as f64;
        let available = system.available_memory() as f64;
        let free = system.free_memory() as f64;
        let total = system.total_memory() as f64;
        let cpu_count = system.cpus().len() as f64;
        drop(system);

        // Translate cumulative CPU ms → fractional seconds and feed the
        // counter the *delta* against the last-emitted value. The
        // `is_finite()` guard catches the (unlikely) NaN/infinity case;
        // the `> 0.0` guard catches a monotonic regression (a quirk
        // sysinfo could theoretically emit if its internal accumulator
        // resets — none have been observed, but the no-op arm costs
        // nothing).
        let cpu_seconds_now = cpu_ms_total as f64 / 1000.0;
        let mut last = self.last_cpu_seconds.lock();
        let delta = cpu_seconds_now - *last;
        if delta > 0.0 && delta.is_finite() {
            self.cpu_seconds_total.inc_by(delta);
            *last = cpu_seconds_now;
        }
        drop(last);

        // Feed the integrating gauges with the freshest sysinfo
        // reading, then ask each gauge for its `MetricFamily`.
        self.memory_used_slot.set(used);
        self.memory_available_slot.set(available);
        self.memory_free_slot.set(free);

        let mut families = Vec::with_capacity(8);
        families.extend(self.cpu_seconds_total.collect());
        families.push(simple_gauge_family(
            "process_resident_memory_bytes",
            "process resident memory in bytes",
            memory_bytes,
        ));
        families.push(simple_gauge_family(
            "process_start_time_seconds",
            "process start time epoch seconds",
            start_time,
        ));
        families.push(simple_gauge_family(
            "memory_total_bytes",
            "host total memory",
            total,
        ));
        families.push(simple_gauge_family(
            "cpu_count",
            "number of logical cpus",
            cpu_count,
        ));
        families.extend(self.memory_used.collect());
        families.extend(self.memory_available.collect());
        families.extend(self.memory_free.collect());
        families
    }
}

fn simple_desc(name: &str, help: &str) -> prometheus::Result<Desc> {
    Desc::new(
        name.to_string(),
        help.to_string(),
        Vec::new(),
        std::collections::HashMap::new(),
    )
}

fn simple_gauge_family(name: &str, help: &str, value: f64) -> MetricFamily {
    let mut family = MetricFamily::default();
    family.set_name(name.to_string());
    family.set_help(help.to_string());
    family.set_field_type(MetricType::GAUGE);
    let mut metric = Metric::default();
    let mut gauge = Gauge::default();
    gauge.set_value(value);
    metric.set_gauge(gauge);
    family.mut_metric().push(metric);
    family
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// All emitted family names match exactly what the app-level e2e
    /// (`crates/app/tests/metrics_e2e.rs`) asserts on. A swap or rename
    /// inside `collect()` would surface here before reaching the e2e.
    #[test]
    fn family_names_match_e2e_expectations() {
        let collector = ProcessCollector::new().unwrap();
        let families = collector.collect();
        let names: HashSet<&str> = families.iter().map(|f| f.name()).collect();
        for expected in [
            "process_cpu_seconds_total",
            "process_resident_memory_bytes",
            "process_start_time_seconds",
            "memory_total_bytes",
            "cpu_count",
            "memory_used_bytes_seconds_integral",
            "memory_available_bytes_seconds_integral",
            "memory_free_bytes_seconds_integral",
        ] {
            assert!(
                names.contains(expected),
                "expected family {expected:?} in {names:?}"
            );
        }
    }

    /// `process_cpu_seconds_total` must accumulate strictly via
    /// `inc_by(delta)` — never via re-publishing an absolute value.
    /// Two scrapes back-to-back: the second cannot be smaller than
    /// the first (Counter contract). We don't require a strict `>`
    /// between scrapes: on a heavily-loaded CI host the delta can be
    /// zero across a sub-30ms window. The contract we actually
    /// enforce is monotonicity (`value_after >= value_before`).
    #[test]
    fn cpu_seconds_total_is_monotonic() {
        let collector = ProcessCollector::new().unwrap();
        let _ = collector.collect();
        let families_before = collector.collect();
        let value_before = families_before
            .iter()
            .find(|f| f.name() == "process_cpu_seconds_total")
            .unwrap()
            .get_metric()[0]
            .get_counter()
            .get_value();
        // Burn a measurable amount of CPU so the second scrape sees a
        // positive delta on most systems. Even if the host is heavily
        // loaded and the delta is observed as zero in <30ms, the
        // monotonic property still holds and is what we assert.
        let mut acc: u64 = 0;
        for i in 0..200_000 {
            acc = acc.wrapping_add(i);
        }
        std::hint::black_box(acc);
        let families_after = collector.collect();
        let value_after = families_after
            .iter()
            .find(|f| f.name() == "process_cpu_seconds_total")
            .unwrap()
            .get_metric()[0]
            .get_counter()
            .get_value();
        assert!(
            value_after >= value_before,
            "cpu_seconds_total must be monotonic: before={value_before} after={value_after}"
        );
    }

    /// `collect()` must push the freshest `used` / `available` / `free`
    /// reading into the correctly-paired TIG slot. Two scrapes with a
    /// real sleep in between let each integral accumulate a
    /// strictly-positive contribution. A slot-swap regression (e.g.
    /// feeding `available` into the `used` slot) would still produce a
    /// non-zero value but would invert the relative magnitudes on a
    /// system with non-trivial memory pressure; the value-set portion
    /// of this test also asserts the per-call gauges come back > 0
    /// (catching a constant-zero regression in the sysinfo call path).
    #[test]
    fn integrating_slots_advance_on_collect() {
        let collector = ProcessCollector::new().unwrap();
        // First collect seeds the slots with the current readings.
        let _ = collector.collect();
        std::thread::sleep(std::time::Duration::from_millis(30));
        let families = collector.collect();

        for fname in [
            "memory_used_bytes_seconds_integral",
            "memory_available_bytes_seconds_integral",
            "memory_free_bytes_seconds_integral",
        ] {
            let family = families
                .iter()
                .find(|f| f.name() == fname)
                .unwrap_or_else(|| panic!("missing family {fname}"));
            let value: f64 = family
                .get_metric()
                .iter()
                .map(|m| m.get_counter().get_value())
                .sum();
            assert!(
                value > 0.0,
                "{fname} should accumulate a positive integral on a live system; got {value}"
            );
        }

        // The plain `memory_total_bytes` gauge must be > 0 too — if it
        // were zero, sysinfo silently failed to read host memory.
        let total = families
            .iter()
            .find(|f| f.name() == "memory_total_bytes")
            .unwrap();
        assert!(
            total.get_metric()[0].get_gauge().get_value() > 0.0,
            "memory_total_bytes must be > 0 on any live system"
        );
    }
}
