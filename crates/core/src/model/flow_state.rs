use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use crate::config::validation::SamplingConfig;
use crate::mapping::ColumnMapping;
use crate::model::{ReadSpec, WriteSpec};
use crate::traits::{Sink, Source, Storage};
use crate::types::{ConversionContext, DataType};

/// A flow with its components built and specs derived from config, but
/// without I/O validation yet. Returned by `validation::pipeline::assemble`.
/// Cannot be passed to the runner — call `validate` first to obtain a
/// `FlowState`.
#[derive(Clone)]
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
    /// Operator-resolved sampling-validation decision (after applying
    /// the per-backend factory default for `Unset`).
    pub sampling: SamplingConfig,
    /// Whether the validation pipeline runs source / storage access
    /// probes for this flow.
    pub access_check: bool,
    /// Whether the validation pipeline runs schema introspection +
    /// matrix + duplicate-`to` checks. For Mongo this is honoured but
    /// only partial (the schema is sampled, not authoritative).
    pub fields_check: bool,
    /// Whether the validation pipeline runs the sink's write probe.
    pub inserts_check: bool,
    /// Persistence strategy for the source's cursor state.
    /// `ColumnCursor` (default) → `Storage::{load,save}_cursor`.
    /// `ResumeToken` → `Storage::{load,save}_resume_token` (used by
    /// CDC sources whose pagination is an opaque BSON blob, not
    /// per-column values).
    pub cursor_persistence: CursorPersistence,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorPersistence {
    #[default]
    ColumnCursor,
    ResumeToken,
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
            source: dt.clone(),
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
/// slice. Tests reach `for_test` via the `flow::test_utils` module.
#[derive(Clone)]
pub struct FlowState {
    inner: AssembledFlow,
    /// Per-column conversion plan. Identity columns get an identity plan;
    /// the runner skips per-cell `convert` calls for them.
    pub conversions: Vec<ConversionPlan>,
    /// Pre-computed positions in `WriteSpec.columns` of the columns
    /// listed in `conflict.key`. Filled at construction time so the
    /// CDC dedup hot path does not redo the column-name → index lookup
    /// for every batch. `None` when there is no `conflict` block or
    /// when any key column is missing from the mapping (the latter is
    /// rejected at validate-time, but we keep the option-shape so the
    /// cache stays robust against future config rewrites). Read via
    /// the `dedup_key_indices()` accessor; intentionally private to
    /// stop callers from manufacturing inconsistent values.
    dedup_key_indices: Option<Vec<usize>>,
}

impl FlowState {
    /// Internal constructor — called from `validation::pipeline::validate`
    /// after schema introspection produces the conversions vector.
    pub(crate) fn new(inner: AssembledFlow, conversions: Vec<ConversionPlan>) -> Self {
        let dedup_key_indices = compute_dedup_key_indices(&inner.write_spec);
        Self {
            inner,
            conversions,
            dedup_key_indices,
        }
    }

    /// Bypasses validation. Test-only — lives behind `cfg(test)` to keep
    /// the type discipline for production callers.
    #[cfg(test)]
    pub fn new_unchecked(inner: AssembledFlow, conversions: Vec<ConversionPlan>) -> Self {
        let dedup_key_indices = compute_dedup_key_indices(&inner.write_spec);
        Self {
            inner,
            conversions,
            dedup_key_indices,
        }
    }

    /// Indices into the row's `values` slice that select the
    /// `conflict.key` columns. Used by the CDC dedup path to feed
    /// `Row::raw_key` without re-deriving the lookup per row. `None`
    /// when the flow has no conflict block — dedup short-circuits in
    /// that case anyway.
    pub fn dedup_key_indices(&self) -> Option<&[usize]> {
        self.dedup_key_indices.as_deref()
    }
}

fn compute_dedup_key_indices(write_spec: &WriteSpec) -> Option<Vec<usize>> {
    let conflict = write_spec.conflict.as_ref()?;
    conflict
        .key
        .iter()
        .map(|k| write_spec.columns.iter().position(|c| c == k))
        .collect::<Option<Vec<_>>>()
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
