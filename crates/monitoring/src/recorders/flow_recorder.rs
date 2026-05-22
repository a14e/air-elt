use std::sync::Arc;
use std::time::Instant;

use prometheus::{IntCounter, IntCounterVec};

use crate::summary::SummarySlot;

/// Per-flow identifiers used to bind labels at recorder construction.
/// Each flow has exactly one source, sink, and storage; the kind
/// (`postgres` / `mongodb` / …) is the component's `type` from the
/// config.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct FlowLabels {
    pub flow: String,
    pub source_name: String,
    pub source_kind: String,
    pub sink_name: String,
    pub sink_kind: String,
    pub storage_name: String,
    pub storage_kind: String,
}

/// Stage on which an error happened. Drives the `stage` label of the
/// errors counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorStage {
    Fetch,
    Transform,
    Sink,
    Storage,
    Other,
}

impl ErrorStage {
    pub fn as_label(self) -> &'static str {
        match self {
            ErrorStage::Fetch => "fetch",
            ErrorStage::Transform => "transform",
            ErrorStage::Sink => "sink",
            ErrorStage::Storage => "storage",
            ErrorStage::Other => "other",
        }
    }
}

/// Row operation classification. Monitoring's own copy of the runner's
/// `RowOp` so the monitoring crate stays decoupled from
/// `air-elt-core`. The runner maps once at the call site.
#[derive(Debug, Clone, Copy)]
pub enum RowOp {
    Upsert,
    Delete,
}

impl RowOp {
    pub fn as_label(self) -> &'static str {
        match self {
            RowOp::Upsert => "upsert",
            RowOp::Delete => "delete",
        }
    }
}

/// Per-flow recorder. Holds a single `Option<Arc<Inner>>` — when
/// `None`, every method is a no-op (monitoring disabled or this is a
/// validation-time stub).
// No `Default` impl — see `PoolStatsRecorder`'s rationale: a silent
// `Default::default()` would hand back a disabled recorder and hide the
// intent at the call site. Callers use [`Self::disabled`] explicitly.
#[derive(Clone)]
pub struct FlowRecorder {
    inner: Option<Arc<FlowRecorderInner>>,
}

pub(crate) struct FlowRecorderInner {
    pub(crate) labels: FlowLabels,
    pub(crate) fetch_slot: SummarySlot,
    pub(crate) transform_slot: SummarySlot,
    pub(crate) sink_slot: SummarySlot,
    pub(crate) rows_read_upsert: IntCounter,
    pub(crate) rows_read_delete: IntCounter,
    pub(crate) rows_written_upsert: IntCounter,
    pub(crate) rows_written_delete: IntCounter,
    pub(crate) rows_skipped_upsert: IntCounter,
    pub(crate) rows_skipped_delete: IntCounter,
    /// Shared with the manager. `with_label_values` resolves the child
    /// counter on demand — prometheus caches the child internally, so a
    /// repeated lookup amortises to a single hash + atomic inc.
    pub(crate) errors: IntCounterVec,
}

impl FlowRecorder {
    pub(crate) fn enabled(inner: FlowRecorderInner) -> Self {
        Self {
            inner: Some(Arc::new(inner)),
        }
    }

    pub fn disabled() -> Self {
        Self { inner: None }
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn start_recording_fetch(&self) -> Timer<'_> {
        Timer::new(self.inner.as_ref().map(|i| &i.fetch_slot))
    }

    pub fn start_recording_transform(&self) -> Timer<'_> {
        Timer::new(self.inner.as_ref().map(|i| &i.transform_slot))
    }

    pub fn start_recording_sink(&self) -> Timer<'_> {
        Timer::new(self.inner.as_ref().map(|i| &i.sink_slot))
    }

    pub fn inc_rows_read(&self, n: u64, op: RowOp) {
        if n == 0 {
            return;
        }
        if let Some(inner) = &self.inner {
            match op {
                RowOp::Upsert => inner.rows_read_upsert.inc_by(n),
                RowOp::Delete => inner.rows_read_delete.inc_by(n),
            }
        }
    }

    pub fn inc_rows_written(&self, n: u64, op: RowOp) {
        if n == 0 {
            return;
        }
        if let Some(inner) = &self.inner {
            match op {
                RowOp::Upsert => inner.rows_written_upsert.inc_by(n),
                RowOp::Delete => inner.rows_written_delete.inc_by(n),
            }
        }
    }

    pub fn inc_rows_skipped(&self, n: u64, op: RowOp) {
        if n == 0 {
            return;
        }
        if let Some(inner) = &self.inner {
            match op {
                RowOp::Upsert => inner.rows_skipped_upsert.inc_by(n),
                RowOp::Delete => inner.rows_skipped_delete.inc_by(n),
            }
        }
    }

    pub fn inc_error(&self, stage: ErrorStage, kind: &str) {
        let Some(inner) = &self.inner else {
            return;
        };
        let (stage_kind, stage_name) = match stage {
            ErrorStage::Fetch => (
                inner.labels.source_kind.as_str(),
                inner.labels.source_name.as_str(),
            ),
            ErrorStage::Sink => (
                inner.labels.sink_kind.as_str(),
                inner.labels.sink_name.as_str(),
            ),
            ErrorStage::Storage => (
                inner.labels.storage_kind.as_str(),
                inner.labels.storage_name.as_str(),
            ),
            ErrorStage::Transform | ErrorStage::Other => ("", ""),
        };
        inner
            .errors
            .with_label_values(&[
                &inner.labels.flow,
                stage.as_label(),
                stage_kind,
                stage_name,
                kind,
            ])
            .inc();
    }
}

/// RAII timer that observes elapsed wall-clock seconds into the
/// targeted summary slot on drop. Disabled timers carry no state.
pub struct Timer<'a> {
    slot: Option<&'a SummarySlot>,
    start: Option<Instant>,
}

impl<'a> Timer<'a> {
    fn new(slot: Option<&'a SummarySlot>) -> Self {
        Self {
            slot,
            start: slot.map(|_| Instant::now()),
        }
    }
}

impl Drop for Timer<'_> {
    fn drop(&mut self) {
        if let (Some(slot), Some(start)) = (self.slot, self.start) {
            slot.observe(start.elapsed().as_secs_f64());
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::summary::Summary;
    use prometheus::core::Collector;
    use prometheus::{IntCounterVec, Opts};
    use std::time::Duration;

    fn make_recorder() -> (FlowRecorder, IntCounterVec) {
        let labels = FlowLabels {
            flow: "f".into(),
            source_name: "src".into(),
            source_kind: "postgres".into(),
            sink_name: "snk".into(),
            sink_kind: "postgres".into(),
            storage_name: "st".into(),
            storage_kind: "postgres".into(),
        };
        let summary = Summary::new(
            "air_elt_fetch_seconds_test",
            "test",
            vec!["flow", "source"],
            Duration::from_secs(60),
            Duration::from_secs(1),
            vec![0.5],
        )
        .unwrap();
        let fetch_slot = summary.allocate(vec!["f".into(), "src".into()]);
        let transform_slot = summary.allocate(vec!["f".into(), "src".into()]);
        let sink_slot = summary.allocate(vec!["f".into(), "src".into()]);

        let rows_total = IntCounterVec::new(
            Opts::new("rows_total_test", "test"),
            &["flow", "stage", "component", "component_kind", "op"],
        )
        .unwrap();
        let rows_read_upsert = rows_total
            .with_label_values(&["f", "read", "src", "postgres", "upsert"])
            .clone();
        let rows_read_delete = rows_total
            .with_label_values(&["f", "read", "src", "postgres", "delete"])
            .clone();
        let rows_written_upsert = rows_total
            .with_label_values(&["f", "written", "snk", "postgres", "upsert"])
            .clone();
        let rows_written_delete = rows_total
            .with_label_values(&["f", "written", "snk", "postgres", "delete"])
            .clone();
        let rows_skipped_upsert = rows_total
            .with_label_values(&["f", "skipped", "snk", "postgres", "upsert"])
            .clone();
        let rows_skipped_delete = rows_total
            .with_label_values(&["f", "skipped", "snk", "postgres", "delete"])
            .clone();

        let errors = IntCounterVec::new(
            Opts::new("errors_total_test", "test"),
            &["flow", "stage", "stage_kind", "stage_name", "kind"],
        )
        .unwrap();

        let recorder = FlowRecorder::enabled(FlowRecorderInner {
            labels,
            fetch_slot,
            transform_slot,
            sink_slot,
            rows_read_upsert,
            rows_read_delete,
            rows_written_upsert,
            rows_written_delete,
            rows_skipped_upsert,
            rows_skipped_delete,
            errors: errors.clone(),
        });
        (recorder, errors)
    }

    fn errors_count(errors: &IntCounterVec, stage: &str) -> u64 {
        let families = errors.collect();
        families
            .iter()
            .flat_map(|f| f.get_metric().iter())
            .filter(|m| {
                m.get_label()
                    .iter()
                    .any(|lp| lp.name() == "stage" && lp.value() == stage)
            })
            .map(|m| m.get_counter().get_value() as u64)
            .sum()
    }

    #[test]
    fn record_error_increments_each_stage_independently() {
        let (recorder, errors) = make_recorder();
        recorder.inc_error(ErrorStage::Fetch, "io");
        recorder.inc_error(ErrorStage::Fetch, "io");
        recorder.inc_error(ErrorStage::Transform, "json");
        recorder.inc_error(ErrorStage::Sink, "constraint");
        recorder.inc_error(ErrorStage::Storage, "lost");

        assert_eq!(errors_count(&errors, "fetch"), 2);
        assert_eq!(errors_count(&errors, "transform"), 1);
        assert_eq!(errors_count(&errors, "sink"), 1);
        assert_eq!(errors_count(&errors, "storage"), 1);
    }

    fn rows_total(recorder: &FlowRecorder) -> u64 {
        // The recorder's Inner shares the IntCounterVec children; sum
        // them via the recorder's inner counters by reading each child.
        let inner = recorder.inner.as_ref().unwrap();
        inner.rows_read_upsert.get()
            + inner.rows_read_delete.get()
            + inner.rows_written_upsert.get()
            + inner.rows_written_delete.get()
            + inner.rows_skipped_upsert.get()
            + inner.rows_skipped_delete.get()
    }

    #[test]
    fn record_rows_zero_is_a_noop() {
        let (recorder, _errors) = make_recorder();
        recorder.inc_rows_read(0, RowOp::Upsert);
        recorder.inc_rows_read(0, RowOp::Delete);
        recorder.inc_rows_written(0, RowOp::Upsert);
        recorder.inc_rows_written(0, RowOp::Delete);
        recorder.inc_rows_skipped(0, RowOp::Upsert);
        recorder.inc_rows_skipped(0, RowOp::Delete);
        assert_eq!(rows_total(&recorder), 0);
    }

    #[test]
    fn record_rows_nonzero_increments() {
        let (recorder, _errors) = make_recorder();
        recorder.inc_rows_read(3, RowOp::Upsert);
        recorder.inc_rows_written(2, RowOp::Upsert);
        recorder.inc_rows_skipped(1, RowOp::Delete);
        assert_eq!(rows_total(&recorder), 6);
    }
}
