use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use air_elt_expr::ExprValue;

use crate::config::expression::ExpressionContext;
use crate::config::validation::SamplingConfig;
use crate::error::ValidationError;
use crate::mapping::{self, ColumnMapping, DirectMapping, ExpandedMapping};
use crate::model::{ConfigReadSpec, ConfigWriteSpec, ReadSpec, Schema, WriteSpec};
use crate::traits::{Sink, Source, Storage};
use crate::transform::SwitchTable;
// Switch tables travel inline on the conversion plan — no Arc, no
// global registry. The plan / Transform are only cloned at validation
// / sampling boundaries, which happens a handful of times per flow
// lifetime; the per-tick path borrows.
use crate::types::{ConversionContext, DataType, Value};

/// A flow with its components built and config-time specs derived, but
/// without I/O validation yet. Returned by `validation::pipeline::assemble`.
/// Cannot be passed to the runner — call `validate` first to obtain a
/// `FlowState`.
///
/// `rules` carries the normalised mapping (post-shorthand, pre-schema
/// expansion). The wildcard fan-out and body-pack synthesis run at
/// validate time inside `mapping::expand` and produce the final
/// `ReadSpec` / `WriteSpec` stored on [`DerivedPlans`]. The
/// `config_*_spec` fields below carry only the schema-independent parts
/// of those specs (table, cursor, limit, etc.) so callers that need
/// `columns` / `needs_body` reach for the derived spec instead.
#[derive(Clone)]
pub struct AssembledFlow {
    pub name: String,
    /// Shared via `Arc` so multiple flows referencing the same source by
    /// name reuse a single instance (and its pool). The connector
    /// crates expose their config-given name + kind through the
    /// `Source` / `Sink` / `Storage` traits; the assemble pipeline
    /// uses those at recorder mint time and we don't cache the strings
    /// on the flow itself.
    pub source: Arc<dyn Source>,
    pub sink: Arc<dyn Sink>,
    pub storage: Arc<dyn Storage>,
    pub rules: Vec<ColumnMapping>,
    pub config_read_spec: ConfigReadSpec,
    pub config_write_spec: ConfigWriteSpec,
    pub interval: Duration,
    /// Resolved per-flow startup jitter ceiling — `min(interval, 5min)`
    /// when the operator omits `cursor.jitter`, or the explicit value
    /// otherwise. Zero disables jitter. The runner shifts the first
    /// tick by `deterministic_hash(flow.name) % jitter` before draining;
    /// subsequent ticks proceed at `interval` cadence as before.
    pub jitter: Duration,
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
    pub cursor_persistence: CursorPersistence,
    /// Per-flow concurrency lock. Built once by
    /// `validation::pipeline::assemble` from the shared
    /// `ConcurrencyManager`. Exposes one `acquire_*` method per
    /// component kind; callers scope the returned permit to exactly
    /// the I/O unit that touches that component. No call site ever
    /// holds more than one permit at a time, so cross-flow deadlock
    /// is structurally impossible — there is no canonical lock order.
    /// See the `project-conventions` skill ("Concurrency:
    /// per-component semaphores").
    pub lock_handle: crate::util::FlowLockHandle,
    /// Per-flow metrics recorder. Minted idempotently by `assemble`
    /// via `MonitoringManager::flow_recorder` — flow runners just
    /// clone it. Disabled when monitoring is off.
    pub recorder: air_elt_monitoring::FlowRecorder,
    /// Expression evaluation context for resolving `default = "env('KEY', 'fallback')"`
    /// style literals. Built once in `assemble` from the registry's
    /// `FunctionRegistry` + the config directory. `None` when expression
    /// evaluation is not available (e.g. tests that build flows without
    /// a config directory).
    pub expr_context: Arc<crate::config::expression::ExpressionContext>,
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
///
/// `source` is `Option<DataType>`: `Some(t)` is the static fast path used
/// when the source side has an authoritative type (`information_schema`
/// for SQL connectors); `None` means "derive at apply time from the
/// actual `Value` variant" and is emitted for schemaless sources whose
/// sampled types are non-authoritative.
///
/// When `switch.is_some()`, the lowered transform op is a
/// `TransformOp::Switch` (no further `Convert` wrapping — switch values
/// are already in the sink's `DataType` after compile-time
/// canonicalisation). `switch` and `ctx.default` are independent — the
/// switch carries its own default (folded into the `SwitchTable`); the
/// plan's `ctx.default` is only consulted on the `Direct` path.
#[derive(Debug, Clone)]
pub struct ColumnConversionPlan {
    pub source: Option<DataType>,
    pub sink: DataType,
    pub ctx: ConversionContext,
    pub switch: Option<SwitchTable>,
}

impl ColumnConversionPlan {
    /// Build an identity plan (no truncate, no default). Test helper.
    pub fn identity(dt: DataType) -> Self {
        Self {
            source: Some(dt.clone()),
            sink: dt,
            ctx: ConversionContext::passthrough(),
            switch: None,
        }
    }

    pub fn is_identity(&self) -> bool {
        self.source.as_ref() == Some(&self.sink)
            && !self.ctx.truncate
            && self.ctx.default.is_none()
            && self.switch.is_none()
    }
}

/// Derived per-flow runtime plans rebuilt together with schema.
///
/// Constructed by [`FlowState::rebuild_derived`] from the static rules
/// stashed on `AssembledFlow` plus the live source/sink schemas. Holds
/// the only fully-populated [`ReadSpec`] / [`WriteSpec`] in the system —
/// the post-expansion shape every connector consumes.
#[derive(Clone)]
pub struct DerivedPlans {
    /// Lowered Transform IR program built from the expanded mapping
    /// plus per-column / per-body conversion plans. The runner calls
    /// `transform.apply(raw_batch)` to produce the final `Batch` for
    /// the sink in one pass — projection, body folding, and per-cell
    /// conversion all happen here. The mongo→mongo `["*"]` raw
    /// passthrough lowers to a single `Body` op writing the `_root`
    /// synthetic target.
    pub transform: crate::transform::Transform,
    /// Post-expansion read spec, finalised once during plan
    /// construction. The runner clones this into the per-tick read
    /// future instead of rebuilding columns every iteration.
    pub read_spec: ReadSpec,
    /// Post-expansion write spec, finalised once during plan
    /// construction. Mirrors `read_spec` on the sink side.
    pub write_spec: WriteSpec,
}

/// Validated flow ready for execution. Owned exclusively by a single
/// `FlowRunner`; concurrent access is not part of the contract — each
/// `tokio::spawn(...)` moves a fresh `FlowState`. `derived` is always
/// populated: the runner rebuilds it in-place on `RuntimeError::Backend`
/// (paired with ctx rebuild) via [`Self::rebuild_derived`].
#[derive(Clone)]
pub struct FlowState {
    inner: AssembledFlow,
    derived: DerivedPlans,
}

impl FlowState {
    /// Constructor — called from `validation::pipeline::validate`
    /// after schema introspection produces the derived plans.
    pub fn new(inner: AssembledFlow, derived: DerivedPlans) -> Self {
        Self { inner, derived }
    }

    /// Borrow the derived plans. Always populated — `rebuild_derived`
    /// overwrites in place rather than clearing.
    pub fn derived(&self) -> &DerivedPlans {
        &self.derived
    }

    /// Rebuild derived plans from the live schemas. Re-runs the
    /// expansion + per-column conversion plan build using the rules
    /// stored on `AssembledFlow`. Called by the runner after a backend
    /// error / fresh ctx build to pick up any schema drift since the
    /// last successful tick.
    pub fn rebuild_derived(&mut self, src: &Schema, dst: &Schema) -> Result<(), ValidationError> {
        self.derived = build_derived_plans(&self.inner, src, dst)?;
        Ok(())
    }
}

/// Build a fresh `DerivedPlans` from the rules + live schemas. Pure —
/// no I/O. Used by [`FlowState::rebuild_derived`] (runner-side rebuild
/// after a backend-error ctx drop). The validation pipeline calls
/// [`build_derived_plans_from_expanded`] directly because it has
/// already paid the `mapping::expand` cost for its own invariants
/// checks — the split avoids a double expansion on the hot validate
/// path while keeping a single source of truth.
pub fn build_derived_plans(
    flow: &AssembledFlow,
    src: &Schema,
    dst: &Schema,
) -> Result<DerivedPlans, ValidationError> {
    let expanded = mapping::expand(
        &flow.rules,
        src,
        dst,
        flow.source.schemaless(),
        flow.sink.schemaless(),
        &flow.name,
    )?;
    // `_from_expanded` still takes `Option<&Schema>` + `dst_schemaless`
    // for its post-expansion field-lookup work; surface them here.
    // `Schemaless` (no sample) collapses to `None`; both `Fixed` and
    // `SchemalessWithSample` surface the carried fields.
    let src_schema = schema_with_fields(src);
    let dst_schema = schema_with_fields(dst);
    let dst_schemaless = dst.is_schemaless();
    build_derived_plans_from_expanded(flow, &expanded, src_schema, dst_schema, dst_schemaless)
}

/// Returns `Some(schema)` when the schema carries fields the downstream
/// stages can index against — fixed (DDL-derived) or schemaless with a
/// sample. Bare schemaless (no sample) collapses to `None`.
fn schema_with_fields(s: &Schema) -> Option<&Schema> {
    use crate::model::SchemaKind;
    match s.kind() {
        SchemaKind::Fixed | SchemaKind::SchemalessWithSample => Some(s),
        SchemaKind::Schemaless => None,
    }
}

/// Plan construction from an already-expanded mapping. Shared between
/// [`build_derived_plans`] (which expands first) and the validation
/// pipeline (which expanded once already for its own invariants and
/// passes the result through here).
pub fn build_derived_plans_from_expanded(
    flow: &AssembledFlow,
    expanded: &ExpandedMapping,
    src_schema: Option<&Schema>,
    dst_schema: Option<&Schema>,
    dst_schemaless: bool,
) -> Result<DerivedPlans, ValidationError> {
    let derived_dst_owned: Option<Schema> = if dst_schema.is_none() && dst_schemaless {
        src_schema.map(|s| derive_schemaless_sink_schema(s, expanded))
    } else {
        None
    };
    let effective_dst = dst_schema.or(derived_dst_owned.as_ref());
    let conversions = build_conversions(flow, expanded, src_schema, effective_dst)?;
    let body_data_type = flow.source.body_data_type();
    let body_conversions = build_body_conversions(
        body_data_type.clone(),
        expanded,
        effective_dst,
        dst_schemaless,
    )?;
    let columns = expanded.read_columns();
    let read_spec = ReadSpec {
        columns: columns.clone(),
        table: flow.config_read_spec.table.clone(),
        cursor_fields: flow.config_read_spec.cursor_fields.clone(),
        cursor_order: flow.config_read_spec.cursor_order,
        limit: flow.config_read_spec.limit,
        source_options: flow.config_read_spec.source_options.clone(),
        // The source attaches `Row.body` only when the flow has a body
        // target — the cost-guard tells it whether to pay per row.
        needs_body: expanded.body.is_some(),
    };
    let write_spec = WriteSpec {
        columns: expanded.write_columns(),
        table: flow.config_write_spec.table.clone(),
        conflict: flow.config_write_spec.conflict.clone(),
    };
    let transform = crate::transform::compile_to_transform(
        expanded,
        body_data_type,
        &conversions,
        &body_conversions,
        &columns,
        flow.source.schemaless(),
    )?;
    Ok(DerivedPlans {
        read_spec,
        write_spec,
        transform,
    })
}

/// Build per-body-target `ColumnConversionPlan`s. The source-side type is
/// taken straight from the source's `body_data_type()`; the sink-side
/// type is whatever the sink's body-target column resolves to. For
/// schemaless sinks (no real `dst_schema`) we fall back to the source
/// body_data_type type itself — identity is correct because the schemaless
/// sink accepts whatever shape the body_data_type produces.
fn build_body_conversions(
    body_data_type: DataType,
    expanded: &ExpandedMapping,
    dst_schema: Option<&Schema>,
    dst_schemaless: bool,
) -> Result<Vec<ColumnConversionPlan>, ValidationError> {
    let body = match &expanded.body {
        Some(b) => b,
        None => return Ok(Vec::new()),
    };
    let mut plans: Vec<ColumnConversionPlan> = Vec::with_capacity(body.targets.len());
    for target in &body.targets {
        let sink_dt = match dst_schema.and_then(|s| s.find(target)) {
            Some(f) => f.data_type.clone(),
            None if dst_schemaless => body_data_type.clone(),
            None => {
                return Err(ValidationError::MissingField {
                    side: "sink",
                    field: target.clone(),
                });
            }
        };
        plans.push(ColumnConversionPlan {
            source: Some(body_data_type.clone()),
            sink: sink_dt,
            ctx: ConversionContext::passthrough(),
            switch: None,
        });
    }
    Ok(plans)
}

/// For schemaless sinks (Mongo) we synthesise the dst schema from the
/// source schema using the post-expansion direct mapping. Each direct
/// mapping's `to` becomes a field whose `data_type` mirrors the source
/// column's; missing source columns are skipped (the wildcard-skip
/// path on `expand` already pruned nullable-missing-source slots, so
/// `find` returning `None` here would mean a programming error
/// upstream).
fn derive_schemaless_sink_schema(src: &Schema, expanded: &ExpandedMapping) -> Schema {
    Schema::new(
        expanded
            .direct
            .iter()
            .filter_map(|d| {
                src.find(&d.from).map(|src_field| crate::model::Field {
                    name: d.to.clone(),
                    // `nullable: true` is hard-coded: this schema feeds a
                    // schemaless sink (Mongo) — the inferred shape is
                    // advisory only, never authoritative DDL — so we
                    // don't pretend to enforce NOT NULL on its behalf.
                    data_type: src_field.data_type.clone(),
                    nullable: true,
                })
            })
            .collect(),
    )
}

/// Resolve a default literal: ExprValue.eval() → check/convert to sink type.
/// All TOML literals, expressions, and interpolations go through the same path.
fn resolve_default_literal(
    literal: &toml::Value,
    sink_dt: &DataType,
    expr_context: &ExpressionContext,
) -> Result<Value, String> {
    let expr_val = ExprValue::from_toml(literal.clone());
    let value = expr_val
        .eval(&expr_context.registry, &expr_context.eval_context)
        .map_err(|e| e.to_string())?;

    crate::config::expression::ensure_sink_compatible(value, sink_dt)
}

/// Build per-column ColumnConversionPlans from the expanded mapping. Mirrors
/// the logic in `validation::pipeline::validate_flow` so a rebuild
/// produces the same plans the pipeline did initially.
fn build_conversions(
    flow: &AssembledFlow,
    expanded: &ExpandedMapping,
    src_schema: Option<&Schema>,
    dst_schema: Option<&Schema>,
) -> Result<Vec<ColumnConversionPlan>, ValidationError> {
    let flow_name = &flow.name;
    let dst_schemaless = flow.sink.schemaless();
    let src_schemaless = flow.source.schemaless();
    let mut plans: Vec<ColumnConversionPlan> = Vec::with_capacity(expanded.direct.len());
    for m in &expanded.direct {
        let DirectMapping {
            from,
            to,
            truncate,
            default_literal,
            switch,
        } = m;
        // Look up the source field from the sample-derived schema, if
        // any. For schemaless sources the sample is non-authoritative —
        // missing field is fine (per-cell dispatch handles it) and the
        // sampled `data_type` is informational only. Typed sources
        // demand a real `information_schema` lookup.
        let src_field_opt = src_schema.and_then(|s| s.find(from));
        let src_field = if src_schemaless {
            src_field_opt
        } else {
            let src = src_schema.ok_or_else(|| ValidationError::AccessFailed {
                component: "source:schema",
                name: flow_name.clone(),
                source: Box::new(crate::error::RuntimeError::Other(format!(
                    "rebuild_derived: missing source schema for direct mapping {from:?} → {to:?}"
                ))),
            })?;
            Some(
                src.find(from)
                    .ok_or_else(|| ValidationError::MissingField {
                        side: "source",
                        field: from.clone(),
                    })?,
            )
        };
        let sink_dt = match dst_schema.and_then(|s| s.find(to)) {
            Some(f) => f.data_type.clone(),
            None => {
                // No sink-side entry. For non-schemaless sinks this is
                // a config bug; for schemaless sinks we should never
                // reach this branch because `build_derived_plans`
                // synthesises a dst schema before delegating here.
                return Err(ValidationError::MissingField {
                    side: "sink",
                    field: to.clone(),
                });
            }
        };
        // `default_literal` without `switch` is the NULL-fallback for the
        // source column; it's only meaningful when the source is
        // nullable. With `switch` present the same literal becomes the
        // switch's miss-fallback (default arm), which fires on EVERY
        // unmatched key regardless of nullability — skip the guard.
        // For schemaless sources the sampled `nullable` flag is non-
        // authoritative (a sample can never prove NOT NULL), so the
        // guard is skipped: any field could legitimately carry NULL.
        if default_literal.is_some()
            && switch.is_none()
            && !src_schemaless
            && let Some(f) = &src_field
            && !f.nullable
        {
            return Err(ValidationError::DefaultOnNotNullSource {
                flow: flow_name.clone(),
                column: from.clone(),
            });
        }
        // Switch path: build the lookup table now. The switch's own
        // default replaces the `ctx.default` slot — the runtime path
        // for Switch never consults `ctx.default`, so we leave it
        // empty here. `truncate` is consumed by `compile_switch` to
        // shorten over-length `Text` / `Bytes` RHS literals.
        //
        // Reject a `switch` without `default` against a NOT NULL sink
        // column: a miss would otherwise emit `Value::Null` at runtime,
        // violating the NOT NULL constraint downstream. The check is
        // skipped on schemaless sinks (no declared nullability).
        if let Some(_spec) = switch
            && default_literal.is_none()
            && !dst_schemaless
            && let Some(sink_field) = dst_schema.and_then(|s| s.find(to))
            && !sink_field.nullable
        {
            return Err(ValidationError::SwitchMissingDefaultForNotNullSink {
                flow: flow_name.clone(),
                column: to.clone(),
            });
        }
        let switch_table = match switch {
            Some(spec) => {
                // Switch RHS canonicalisation needs a source `DataType`.
                // For typed sources the sample is the contract; for
                // schemaless sources fall back to the sink type — the
                // switch operates on canonical keys (`Key::from_value`
                // dispatches on the actual `Value` at runtime), so the
                // declared source type only narrows literal parsing.
                let src_dt = src_field
                    .as_ref()
                    .map(|f| f.data_type.clone())
                    .unwrap_or_else(|| sink_dt.clone());
                Some(crate::transform::compile_switch(
                    flow_name,
                    to,
                    &spec.cases,
                    default_literal.as_ref(),
                    *truncate,
                    &src_dt,
                    &sink_dt,
                    dst_schemaless,
                    &flow.expr_context,
                )?)
            }
            None => None,
        };
        let parsed_default = if switch.is_some() {
            None
        } else {
            match default_literal {
                Some(lit) => Some(
                    resolve_default_literal(lit, &sink_dt, &flow.expr_context).map_err(
                        |reason| ValidationError::DefaultEval {
                            flow: flow_name.clone(),
                            column: from.clone(),
                            reason,
                        },
                    )?,
                ),
                None => None,
            }
        };
        let mut ctx = ConversionContext::passthrough();
        ctx.truncate = *truncate;
        ctx.default = parsed_default;
        // For schemaless sources the sampled `source` type is non-
        // authoritative — we set `plan.source = None` so the unified
        // `TransformOp::Convert` dispatches on the actual `Value`
        // variant per cell at apply time. For typed sources we record
        // the resolved source `DataType` so the static fast path can
        // skip per-cell type resolution.
        let plan_source_dt = if src_schemaless {
            None
        } else {
            Some(
                src_field
                    .as_ref()
                    .map(|f| f.data_type.clone())
                    .unwrap_or_else(|| sink_dt.clone()),
            )
        };
        plans.push(ColumnConversionPlan {
            source: plan_source_dt,
            sink: sink_dt,
            ctx,
            switch: switch_table,
        });
    }
    // Body / wildcard-pack outputs are produced by `TransformOp::Body`
    // ops in the compiled program — they read `Row.body` populated by
    // the source (when `ReadSpec.needs_body == true`) and write into the
    // sink's row slot at the matching position. No `ColumnConversionPlan`
    // is emitted for them; per-body-target plans, if any, are added
    // separately by `build_body_conversions` above.
    Ok(plans)
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

/// Lightweight `Debug` so callers (notably tests) can `unwrap_err()` on
/// `Result<FlowState, ValidationError>`. The wrapped `AssembledFlow`
/// holds `Arc<dyn Source>` etc. which are not `Debug` themselves; only
/// the flow name is printed.
impl std::fmt::Debug for FlowState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowState")
            .field("name", &self.inner.name)
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::any::Any;

    use super::*;
    use crate::config::model::CursorOrder;
    use crate::mapping::ColumnMapping;
    use crate::model::{Field, Schema};
    use crate::traits::{MockSink, MockSource, MockStorage};
    use crate::util::ConcurrencyManager;

    fn assembled(rules: Vec<ColumnMapping>) -> AssembledFlow {
        let mut src = MockSource::new();
        src.expect_schemaless().return_const(false);
        src.expect_body_data_type()
            .returning(|| crate::types::DataType::Json);
        src.expect_name().return_const("test-source".to_string());
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        AssembledFlow {
            name: "test".into(),
            source: Arc::new(src),
            sink: Arc::new(sink),
            storage: Arc::new(MockStorage::new()),
            rules,
            config_read_spec: ConfigReadSpec {
                table: "t".into(),
                cursor_fields: Vec::new(),
                cursor_order: CursorOrder::Asc,
                limit: 1,
                source_options: toml::Table::new(),
            },
            config_write_spec: ConfigWriteSpec {
                table: "t".into(),
                conflict: None,
            },
            interval: Duration::from_millis(10),
            jitter: Duration::ZERO,
            query_timeout: Duration::from_secs(5),
            sampling: SamplingConfig::Disabled,
            access_check: false,
            fields_check: true,
            inserts_check: false,
            cursor_persistence: CursorPersistence::ColumnCursor,
            lock_handle: {
                let mut m = ConcurrencyManager::new();
                m.register_source("test-source", u32::MAX);
                m.register_sink("test-sink", u32::MAX);
                m.register_storage("test-storage", u32::MAX);
                m.handle(
                    "test-source",
                    "test-sink",
                    "test-storage",
                    &mut air_elt_monitoring::MonitoringManager::disabled(),
                )
            },
            recorder: air_elt_monitoring::FlowRecorder::disabled(),
            expr_context: Arc::new(ExpressionContext::new(
                Arc::new(air_elt_expr_funcs::FunctionRegistry::with_builtins()),
                std::path::Path::new("/tmp"),
            )),
        }
    }

    fn schema(fields: &[(&str, DataType, bool)]) -> Schema {
        Schema::new(
            fields
                .iter()
                .map(|(n, dt, nul)| Field {
                    name: (*n).into(),
                    data_type: dt.clone(),
                    nullable: *nul,
                })
                .collect(),
        )
    }

    /// `rebuild_derived` overwrites in place; `derived()` always returns
    /// the current plans.
    #[test]
    fn rebuild_overwrites_in_place() {
        let flow = assembled(vec![ColumnMapping::Direct {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: None,
        }]);
        let initial = build_derived_plans(
            &flow,
            &schema(&[("a", DataType::Int32, false)]),
            &schema(&[("a", DataType::Int32, false)]),
        )
        .unwrap();
        let mut state = FlowState::new(flow, initial);
        let src = schema(&[("a", DataType::Int32, false)]);
        let dst = schema(&[("a", DataType::Int32, false)]);
        state.rebuild_derived(&src, &dst).unwrap();
        let derived = state.derived();
        assert_eq!(derived.transform.cols.len(), 1);
        assert_eq!(derived.read_spec.columns, vec!["a".to_string()]);
        assert_eq!(derived.write_spec.columns, vec!["a".to_string()]);
    }

    /// Body rebuild flips `needs_body` so the source attaches a
    /// body_data_type per row, appends body source columns to
    /// `read_spec.columns`, and lowers to a Transform with one `Take`
    /// + one `Body` op.
    #[test]
    fn rebuild_with_body_produces_transform() {
        let flow = assembled(vec![
            ColumnMapping::Direct {
                from: "id".into(),
                to: "id".into(),
                truncate: false,
                default_literal: None,
            },
            ColumnMapping::Body { to: "body".into() },
        ]);
        let src = schema(&[
            ("id", DataType::Int64, false),
            ("name", DataType::Text { size: None }, false),
        ]);
        let dst = schema(&[
            ("id", DataType::Int64, false),
            ("body", DataType::Json, false),
        ]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        assert_eq!(
            plans.read_spec.columns,
            vec!["id".to_string(), "name".into()],
        );
        assert_eq!(
            plans.write_spec.columns,
            vec!["id".to_string(), "body".into()]
        );
        assert!(plans.read_spec.needs_body);
        assert_eq!(
            plans.transform.cols.len(),
            2,
            "expected one Take + one Body"
        );
        assert!(matches!(
            &plans.transform.cols[0],
            crate::transform::TransformOp::Take { source_index: 0 }
        ));
        assert!(matches!(
            &plans.transform.cols[1],
            crate::transform::TransformOp::Body
        ));
    }

    /// Non-body flows must keep `needs_body=false` so mongo sources
    /// pay no per-row document clone.
    #[test]
    fn rebuild_without_body_leaves_needs_body_false() {
        let flow = assembled(vec![ColumnMapping::Direct {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: None,
        }]);
        let plans = build_derived_plans(
            &flow,
            &schema(&[("a", DataType::Int32, false)]),
            &schema(&[("a", DataType::Int32, false)]),
        )
        .unwrap();
        assert!(!plans.read_spec.needs_body);
    }

    /// Schemaless-both `["*"]` flow: lowers to a Transform with one
    /// `Body` op writing the synthetic `_root` target. `read_spec`
    /// stays empty (no per-column projection); `write_spec` carries one
    /// `_root` column. `needs_body` is `true` because the body block
    /// is present.
    #[test]
    fn rebuild_root_body_flow_emits_single_body_op() {
        use crate::types::dynamic::DynType;
        // A minimal object-shaped custom DynType so the source can
        // advertise `body_data_type().is_object() == true` without
        // pulling in commons-mongodb here.
        #[derive(Debug)]
        struct ObjectyType;
        impl DynType for ObjectyType {
            fn as_any(&self) -> &dyn Any {
                self
            }

            fn kind(&self) -> &str {
                "test.objecty"
            }
            fn is_object(&self) -> bool {
                true
            }
            fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
                false
            }
            fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
                false
            }
            fn convert(
                &self,
                _v: crate::types::Value,
                _t: &DataType,
                _ctx: &ConversionContext,
            ) -> Result<crate::types::Value, crate::types::convert::ConvertError> {
                unreachable!()
            }
            fn construct(
                &self,
                _v: crate::types::Value,
                _t: &DataType,
                _ctx: &ConversionContext,
            ) -> Result<crate::types::Value, crate::types::convert::ConvertError> {
                unreachable!()
            }
            fn clone_box(&self) -> Box<dyn DynType> {
                Box::new(ObjectyType)
            }
        }
        let mut src = MockSource::new();
        src.expect_schemaless().return_const(true);
        src.expect_body_data_type()
            .returning(|| DataType::Custom(Box::new(ObjectyType)));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(true);
        let mut flow = assembled(vec![ColumnMapping::Wildcard]);
        flow.source = Arc::new(src);
        flow.sink = Arc::new(sink);
        let plans =
            build_derived_plans(&flow, &Schema::schemaless(), &Schema::schemaless()).unwrap();
        assert_eq!(plans.transform.cols.len(), 1);
        assert!(matches!(
            &plans.transform.cols[0],
            crate::transform::TransformOp::Body
        ));
        assert!(plans.read_spec.columns.is_empty());
        assert_eq!(
            plans.write_spec.columns,
            vec![crate::mapping::ROOT_BODY_TARGET.to_string()]
        );
        assert!(plans.read_spec.needs_body);
    }

    /// Nullable sink columns missing from the source are omitted
    /// entirely from the expansion — neither in `read_spec_columns`
    /// nor in `write_spec_columns`. The sink writes the row without
    /// them and pg/mysql will use the column's DDL default / NULL.
    #[test]
    fn nullable_missing_source_column_omitted_from_both_specs() {
        let flow = assembled(vec![ColumnMapping::Wildcard]);
        let src = schema(&[("a", DataType::Int32, false)]);
        let dst = schema(&[("a", DataType::Int32, false), ("b", DataType::Int32, true)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        assert_eq!(plans.read_spec.columns, vec!["a".to_string()]);
        assert_eq!(plans.write_spec.columns, vec!["a".to_string()]);
        assert_eq!(plans.transform.cols.len(), 1);
    }

    fn switch_case(key: &str, value: &str) -> crate::mapping::SwitchCase {
        crate::mapping::SwitchCase {
            key: key.into(),
            value: toml::Value::String(value.into()),
        }
    }

    /// Switch without `default` against a NOT NULL sink column must be
    /// rejected at validate-time: a key miss would otherwise emit
    /// `Value::Null` at runtime and slam into the NOT NULL constraint
    /// downstream. Guard lives at `flow_state.rs::build_conversions`.
    #[test]
    fn switch_without_default_rejected_against_not_null_sink() {
        let flow = assembled(vec![ColumnMapping::Switch {
            from: "code".into(),
            to: "label".into(),
            truncate: false,
            cases: vec![switch_case("1", "open"), switch_case("2", "closed")],
            default_literal: None,
        }]);
        let src = schema(&[("code", DataType::Int32, false)]);
        let dst = schema(&[("label", DataType::Text { size: None }, false)]);
        let res = build_derived_plans(&flow, &src, &dst);
        match res {
            Err(ValidationError::SwitchMissingDefaultForNotNullSink { flow, column }) => {
                assert_eq!(flow, "test");
                assert_eq!(column, "label");
            }
            Err(other) => panic!("expected SwitchMissingDefaultForNotNullSink, got {other:?}"),
            Ok(_) => panic!("expected SwitchMissingDefaultForNotNullSink, got Ok"),
        }
    }

    /// Same shape but with `default = "unknown"` is accepted — the
    /// default arm covers the miss case so NOT NULL is safe.
    #[test]
    fn switch_with_default_accepted_against_not_null_sink() {
        let flow = assembled(vec![ColumnMapping::Switch {
            from: "code".into(),
            to: "label".into(),
            truncate: false,
            cases: vec![switch_case("1", "open"), switch_case("2", "closed")],
            default_literal: Some(toml::Value::String("unknown".into())),
        }]);
        let src = schema(&[("code", DataType::Int32, false)]);
        let dst = schema(&[("label", DataType::Text { size: None }, false)]);
        build_derived_plans(&flow, &src, &dst).expect("default arm bridges NOT NULL sink");
    }

    /// `default = …` on a NOT NULL source column is rejected: the
    /// default would never fire (the source has no NULL to substitute
    /// for), so the literal is dead code. Guard lives at
    /// `flow_state.rs::build_conversions` next to the switch case.
    #[test]
    fn default_on_not_null_source_rejected() {
        let flow = assembled(vec![ColumnMapping::Direct {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: Some(toml::Value::Integer(0)),
        }]);
        let src = schema(&[("a", DataType::Int32, false)]);
        let dst = schema(&[("a", DataType::Int32, false)]);
        let res = build_derived_plans(&flow, &src, &dst);
        match res {
            Err(ValidationError::DefaultOnNotNullSource { flow, column }) => {
                assert_eq!(flow, "test");
                assert_eq!(column, "a");
            }
            Err(other) => panic!("expected DefaultOnNotNullSource, got {other:?}"),
            Ok(_) => panic!("expected DefaultOnNotNullSource, got Ok"),
        }
    }

    /// Same default on a NULLABLE source is accepted — that's the
    /// canonical NULL-bridge use case.
    #[test]
    fn default_on_nullable_source_accepted() {
        let flow = assembled(vec![ColumnMapping::Direct {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: Some(toml::Value::Integer(0)),
        }]);
        let src = schema(&[("a", DataType::Int32, true)]);
        let dst = schema(&[("a", DataType::Int32, false)]);
        build_derived_plans(&flow, &src, &dst).expect("nullable source admits default bridge");
    }

    /// `default = …` on a NOT NULL source is **allowed when combined
    /// with `switch`** — for switch the default is the miss-fallback,
    /// which fires on every unmatched key regardless of source
    /// nullability. The guard explicitly excludes switch entries.
    #[test]
    fn default_on_not_null_source_with_switch_accepted() {
        let flow = assembled(vec![ColumnMapping::Switch {
            from: "code".into(),
            to: "label".into(),
            truncate: false,
            cases: vec![switch_case("1", "open"), switch_case("2", "closed")],
            default_literal: Some(toml::Value::String("unknown".into())),
        }]);
        let src = schema(&[("code", DataType::Int32, false)]);
        let dst = schema(&[("label", DataType::Text { size: None }, false)]);
        build_derived_plans(&flow, &src, &dst)
            .expect("switch's default is miss-fallback, not NULL-bridge");
    }

    /// Switch without `default` against a NULLABLE sink column is
    /// accepted: a miss producing `Value::Null` is valid for nullable
    /// columns.
    #[test]
    fn switch_without_default_accepted_against_nullable_sink() {
        let flow = assembled(vec![ColumnMapping::Switch {
            from: "code".into(),
            to: "label".into(),
            truncate: false,
            cases: vec![switch_case("1", "open"), switch_case("2", "closed")],
            default_literal: None,
        }]);
        let src = schema(&[("code", DataType::Int32, false)]);
        let dst = schema(&[("label", DataType::Text { size: None }, true)]);
        build_derived_plans(&flow, &src, &dst).expect("nullable sink admits NULL on miss");
    }

    /// Expression in `default` is evaluated when `expr_context` is set.
    #[test]
    fn expression_default_is_evaluated() {
        use crate::transform::TransformOp;
        use air_elt_expr_funcs::FunctionRegistry;
        use std::path::PathBuf;

        let expr_ctx = Arc::new(crate::config::expression::ExpressionContext::new(
            Arc::new(FunctionRegistry::with_builtins()),
            &PathBuf::from("/tmp"),
        ));
        let mut flow = assembled(vec![ColumnMapping::Direct {
            from: "name".into(),
            to: "name".into(),
            truncate: false,
            default_literal: Some(toml::Value::String(
                "concat('hello', ' ', 'world')".to_string(),
            )),
        }]);
        flow.expr_context = expr_ctx;

        let src = schema(&[("name", DataType::Text { size: None }, true)]);
        let dst = schema(&[("name", DataType::Text { size: None }, false)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        let default_val = match &plans.transform.cols[0] {
            TransformOp::Convert { plan, .. } => plan.ctx.default.as_ref().unwrap(),
            other => panic!("expected Convert op, got {other:?}"),
        };
        assert_eq!(*default_val, Value::Text("hello world".to_string()));
    }

    /// Interpolation in `default` is evaluated via ExprValue.
    #[test]
    fn interpolation_default_is_evaluated() {
        use crate::transform::TransformOp;
        use air_elt_expr_funcs::FunctionRegistry;
        use std::path::PathBuf;

        let expr_ctx = Arc::new(crate::config::expression::ExpressionContext::new(
            Arc::new(FunctionRegistry::with_builtins()),
            &PathBuf::from("/tmp"),
        ));
        let mut flow = assembled(vec![ColumnMapping::Direct {
            from: "name".into(),
            to: "name".into(),
            truncate: false,
            default_literal: Some(toml::Value::String("prefix_{1 + 2}_suffix".to_string())),
        }]);
        flow.expr_context = expr_ctx;

        let src = schema(&[("name", DataType::Text { size: None }, true)]);
        let dst = schema(&[("name", DataType::Text { size: None }, false)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        let default_val = match &plans.transform.cols[0] {
            TransformOp::Convert { plan, .. } => plan.ctx.default.as_ref().unwrap(),
            other => panic!("expected Convert op, got {other:?}"),
        };
        assert_eq!(*default_val, Value::Text("prefix_3_suffix".to_string()));
    }

    /// Plain string literal evaluates to Text directly.
    #[test]
    fn plain_string_default_without_expr_context() {
        use crate::transform::TransformOp;

        let flow = assembled(vec![ColumnMapping::Direct {
            from: "name".into(),
            to: "name".into(),
            truncate: false,
            default_literal: Some(toml::Value::String("hello".to_string())),
        }]);
        let src = schema(&[("name", DataType::Text { size: None }, true)]);
        let dst = schema(&[("name", DataType::Text { size: None }, false)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        let default_val = match &plans.transform.cols[0] {
            TransformOp::Convert { plan, .. } => plan.ctx.default.as_ref().unwrap(),
            other => panic!("expected Convert op, got {other:?}"),
        };
        assert_eq!(*default_val, Value::Text("hello".to_string()));
    }

    /// Expression with explicit cast produces the exact sink type.
    #[test]
    fn expression_default_with_cast_to_float64() {
        use crate::transform::TransformOp;
        use air_elt_expr_funcs::FunctionRegistry;
        use std::path::PathBuf;

        let expr_ctx = Arc::new(crate::config::expression::ExpressionContext::new(
            Arc::new(FunctionRegistry::with_builtins()),
            &PathBuf::from("/tmp"),
        ));
        let mut flow = assembled(vec![ColumnMapping::Direct {
            from: "val".into(),
            to: "val".into(),
            truncate: false,
            default_literal: Some(toml::Value::String("add(1, 2)".to_string())),
        }]);
        flow.expr_context = expr_ctx;

        let src = schema(&[("val", DataType::Float64, true)]);
        let dst = schema(&[("val", DataType::Float64, false)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        let default_val = match &plans.transform.cols[0] {
            TransformOp::Convert { plan, .. } => plan.ctx.default.as_ref().unwrap(),
            other => panic!("expected Convert op, got {other:?}"),
        };
        assert_eq!(*default_val, Value::Float64(3.0));
    }

    /// TOML integer literal is narrowed to Int16 when value fits.
    #[test]
    fn literal_integer_narrowed_to_int16() {
        use crate::transform::TransformOp;

        let flow = assembled(vec![ColumnMapping::Direct {
            from: "val".into(),
            to: "val".into(),
            truncate: false,
            default_literal: Some(toml::Value::Integer(42)),
        }]);
        let src = schema(&[("val", DataType::Int16, true)]);
        let dst = schema(&[("val", DataType::Int16, false)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        let default_val = match &plans.transform.cols[0] {
            TransformOp::Convert { plan, .. } => plan.ctx.default.as_ref().unwrap(),
            other => panic!("expected Convert op, got {other:?}"),
        };
        assert_eq!(*default_val, Value::Int16(42));
    }

    /// TOML integer literal exceeding Int8 range is rejected.
    #[test]
    fn literal_integer_rejected_when_out_of_range() {
        let flow = assembled(vec![ColumnMapping::Direct {
            from: "val".into(),
            to: "val".into(),
            truncate: false,
            default_literal: Some(toml::Value::Integer(300)),
        }]);
        let src = schema(&[("val", DataType::Int8, true)]);
        let dst = schema(&[("val", DataType::Int8, false)]);
        let res = build_derived_plans(&flow, &src, &dst);
        assert!(res.is_err(), "300 should be out of range for Int8");
    }

    /// Expression with explicit cast to Int32 from text.
    #[test]
    fn expression_text_to_int32_via_cast() {
        use crate::transform::TransformOp;
        use air_elt_expr_funcs::FunctionRegistry;
        use std::path::PathBuf;

        let expr_ctx = Arc::new(crate::config::expression::ExpressionContext::new(
            Arc::new(FunctionRegistry::with_builtins()),
            &PathBuf::from("/tmp"),
        ));
        let mut flow = assembled(vec![ColumnMapping::Direct {
            from: "val".into(),
            to: "val".into(),
            truncate: false,
            default_literal: Some(toml::Value::String("toInt32(30)".to_string())),
        }]);
        flow.expr_context = expr_ctx;

        let src = schema(&[("val", DataType::Int32, true)]);
        let dst = schema(&[("val", DataType::Int32, false)]);
        let plans = build_derived_plans(&flow, &src, &dst).unwrap();
        let default_val = match &plans.transform.cols[0] {
            TransformOp::Convert { plan, .. } => plan.ctx.default.as_ref().unwrap(),
            other => panic!("expected Convert op, got {other:?}"),
        };
        assert_eq!(*default_val, Value::Int32(30));
    }
}
