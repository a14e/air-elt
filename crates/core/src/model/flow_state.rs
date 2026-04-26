use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use crate::mapping::ColumnMapping;
use crate::model::{ReadSpec, WriteSpec};
use crate::traits::{Sink, Source, Storage};
use crate::types::{ConversionContext, DataType};

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

/// Per-column conversion plan. Carries the resolved source/sink `DataType`
/// pair plus the per-mapping `ConversionContext` (truncate flag + parsed
/// default value). Built by `validation::pipeline::validate` after schema
/// introspection.
#[derive(Debug, Clone)]
pub struct ConversionPlan {
    pub source: DataType,
    pub sink: DataType,
    pub ctx: ConversionContext,
}

impl ConversionPlan {
    /// Build an identity plan (no truncate, no default). Test helper.
    pub fn identity(dt: DataType) -> Self {
        Self {
            source: dt,
            sink: dt,
            ctx: ConversionContext::passthrough(),
        }
    }

    pub fn is_identity(&self) -> bool {
        self.source == self.sink && !self.ctx.truncate && self.ctx.default.is_none()
    }
}

/// Validated flow ready for execution: I/O probes succeeded, schemas were
/// introspected, and per-column conversions are precomputed. Constructed
/// only by `validation::pipeline::validate` — there is no public
/// constructor that lets you build one with a stale or empty `conversions`
/// slice. Tests reach `for_test` via the `flow::test_support` module.
pub struct FlowState {
    inner: AssembledFlow,
    /// Per-column conversion plan. Identity columns get an identity plan;
    /// the runner skips per-cell `convert` calls for them.
    pub conversions: Vec<ConversionPlan>,
}

impl FlowState {
    /// Internal constructor — called from `validation::pipeline::validate`
    /// after schema introspection produces the conversions vector.
    pub(crate) fn new(inner: AssembledFlow, conversions: Vec<ConversionPlan>) -> Self {
        Self { inner, conversions }
    }

    /// Bypasses validation. Test-only — lives behind `cfg(test)` to keep
    /// the type discipline for production callers.
    #[cfg(test)]
    pub fn new_unchecked(inner: AssembledFlow, conversions: Vec<ConversionPlan>) -> Self {
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
