use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use crate::mapping::ColumnMapping;
use crate::model::{ReadSpec, WriteSpec};
use crate::traits::{Sink, Source, Storage};
use crate::types::DataType;

/// A flow with its components built and specs derived from config, but
/// without I/O validation yet. Returned by `validation::pipeline::assemble`.
/// Cannot be passed to the runner — call `validate` first to obtain a
/// `FlowState`.
pub struct AssembledFlow {
    pub name: String,
    /// Shared via `Arc` so multiple flows referencing the same source by
    /// name reuse a single instance (and its pool).
    pub source: Arc<dyn Source>,
    pub sink: Arc<dyn Sink>,
    pub storage: Arc<dyn Storage>,
    pub mappings: Vec<ColumnMapping>,
    pub read_spec: ReadSpec,
    pub write_spec: WriteSpec,
    pub interval: Duration,
    pub query_timeout: Duration,
}

/// Validated flow ready for execution: I/O probes succeeded, schemas were
/// introspected, and per-column conversions are precomputed. Constructed
/// only by `validation::pipeline::validate` — there is no public
/// constructor that lets you build one with a stale or empty `conversions`
/// slice. Tests reach `for_test` via the `flow::test_support` module.
pub struct FlowState {
    inner: AssembledFlow,
    /// Per-column `(source_dt, sink_dt)`. Identity columns get
    /// `(dt, dt)`; the runner skips conversion for them.
    pub conversions: Vec<(DataType, DataType)>,
}

impl FlowState {
    /// Internal constructor — called from `validation::pipeline::validate`
    /// after schema introspection produces the conversions vector.
    pub(crate) fn new(inner: AssembledFlow, conversions: Vec<(DataType, DataType)>) -> Self {
        Self { inner, conversions }
    }

    /// Bypasses validation. Test-only — lives behind `cfg(test)` to keep
    /// the type discipline for production callers.
    #[cfg(test)]
    pub fn new_unchecked(inner: AssembledFlow, conversions: Vec<(DataType, DataType)>) -> Self {
        Self { inner, conversions }
    }
}

/// Field access on `FlowState` reads through to the assembled flow — the
/// runner sees the same fields it always saw (`flow.name`, `flow.source`,
/// etc.) without needing to know about the wrapper.
impl Deref for FlowState {
    type Target = AssembledFlow;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
