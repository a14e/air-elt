use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use crate::config::validation::SamplingConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping::{self, ColumnMapping, DirectMapping, ExpandedMapping};
use crate::model::{ReadSpec, Schema, WriteSpec};
use crate::traits::{Sink, Source, Storage};
use crate::types::{ConversionContext, DataType};

/// A flow with its components built and specs derived from config, but
/// without I/O validation yet. Returned by `validation::pipeline::assemble`.
/// Cannot be passed to the runner — call `validate` first to obtain a
/// `FlowState`.
///
/// `rules` carries the normalised mapping (post-shorthand, pre-schema
/// expansion). The wildcard fan-out and body-pack synthesis run at
/// validate time inside `mapping::expand`. The resulting `read_spec` /
/// `write_spec` columns are populated from the expanded direct vector.
#[derive(Clone)]
pub struct AssembledFlow {
    pub name: String,
    /// Shared via `Arc` so multiple flows referencing the same source by
    /// name reuse a single instance (and its pool).
    pub source: Arc<dyn Source>,
    pub sink: Arc<dyn Sink>,
    pub storage: Arc<dyn Storage>,
    pub rules: Vec<ColumnMapping>,
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
pub struct ColumnConversionPlan {
    pub source: DataType,
    pub sink: DataType,
    pub ctx: ConversionContext,
}

impl ColumnConversionPlan {
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

/// Derived per-flow runtime plans rebuilt together with schema.
///
/// Constructed by [`FlowState::rebuild_derived`] from the static rules
/// stashed on `AssembledFlow` plus the live source/sink schemas. Cleared
/// on backend errors (alongside the ctx Arcs) so the next tick rebuilds
/// against fresh schemas.
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
/// `tokio::spawn(...)` moves a fresh `FlowState`. The `derived` plans
/// are kept inside `Option` so the runner can drop them on
/// `RuntimeError::Backend` (paired with ctx drop) and rebuild the next
/// tick after `build_context` populates fresh schemas.
#[derive(Clone)]
pub struct FlowState {
    inner: AssembledFlow,
    derived: Option<DerivedPlans>,
}

impl FlowState {
    /// Internal constructor — called from `validation::pipeline::validate`
    /// after schema introspection produces the conversions vector.
    pub(crate) fn new(inner: AssembledFlow, derived: DerivedPlans) -> Self {
        Self {
            inner,
            derived: Some(derived),
        }
    }

    /// Bypasses validation. Test-only — lives behind `cfg(test)` to keep
    /// the type discipline for production callers.
    #[cfg(test)]
    pub fn new_unchecked(inner: AssembledFlow, _conversions: Vec<ColumnConversionPlan>) -> Self {
        let read_spec = inner.read_spec.clone();
        let write_spec = inner.write_spec.clone();
        Self {
            inner,
            derived: Some(DerivedPlans {
                read_spec,
                write_spec,
                transform: crate::transform::Transform::new(Vec::new()),
            }),
        }
    }

    /// Borrow the derived plans. Errors when derived has been cleared
    /// by a prior `invalidate_derived()` and not yet rebuilt — the
    /// caller is expected to call `rebuild_derived(...)` first.
    pub fn derived(&self) -> Result<&DerivedPlans, RuntimeError> {
        self.derived
            .as_ref()
            .ok_or_else(|| RuntimeError::DerivedPlansNotBuilt {
                flow: self.inner.name.clone(),
            })
    }

    /// Drop the cached derived plans. Paired with the runner's ctx-drop
    /// on `RuntimeError::Backend` so the next tick rebuilds against the
    /// fresh schemas exposed by the rebuilt ctx.
    pub fn invalidate_derived(&mut self) {
        self.derived = None;
    }

    /// `true` iff [`derived`] currently holds plans. Used by the runner
    /// to decide whether to call `rebuild_derived` after building the
    /// ctxs on a fresh tick.
    pub fn has_derived(&self) -> bool {
        self.derived.is_some()
    }

    /// Rebuild derived plans from the live schemas. Replaces whatever
    /// is currently in `derived` (`Some` or `None`). Re-runs the
    /// expansion + per-column conversion plan build using the rules
    /// stored on `AssembledFlow`.
    pub fn rebuild_derived(&mut self, src: &Schema, dst: &Schema) -> Result<(), ValidationError> {
        let plans = build_derived_plans(&self.inner, src, dst)?;
        self.derived = Some(plans);
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
    let mut read_spec = flow.read_spec.clone();
    read_spec.columns = expanded.read_columns();
    // The source attaches `RawRow.body` only when the flow has a body
    // target — the cost-guard tells it whether to pay per row.
    read_spec.needs_body = expanded.body.is_some();
    let mut write_spec = flow.write_spec.clone();
    write_spec.columns = expanded.write_columns();
    let transform = crate::transform::compile_to_transform(
        expanded,
        body_data_type,
        &conversions,
        &body_conversions,
        &read_spec.columns,
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
            source: body_data_type.clone(),
            sink: sink_dt,
            ctx: ConversionContext::passthrough(),
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
    let mut plans: Vec<ColumnConversionPlan> = Vec::with_capacity(expanded.direct.len());
    for m in &expanded.direct {
        let DirectMapping {
            from,
            to,
            truncate,
            default_literal,
        } = m;
        let src = src_schema.ok_or_else(|| ValidationError::AccessFailed {
            component: "source:schema",
            name: flow_name.clone(),
            source: Box::new(RuntimeError::Other(format!(
                "rebuild_derived: missing source schema for direct mapping {from:?} → {to:?}"
            ))),
        })?;
        let src_field = src
            .find(from)
            .ok_or_else(|| ValidationError::MissingField {
                side: "source",
                field: from.clone(),
            })?;
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
        if default_literal.is_some() && !src_field.nullable {
            return Err(ValidationError::DefaultOnNotNullSource {
                flow: flow_name.clone(),
                column: from.clone(),
            });
        }
        let parsed_default =
            match default_literal {
                Some(lit) => Some(crate::types::default_value::parse(lit, &sink_dt).map_err(
                    |e| ValidationError::DefaultParse {
                        flow: flow_name.clone(),
                        column: from.clone(),
                        source: e,
                    },
                )?),
                None => None,
            };
        let mut ctx = ConversionContext::passthrough();
        ctx.truncate = *truncate;
        ctx.default = parsed_default;
        plans.push(ColumnConversionPlan {
            source: src_field.data_type.clone(),
            sink: sink_dt,
            ctx,
        });
    }
    // Body / wildcard-pack outputs land in `Row.computed`, which
    // `apply_conversions` never walks — no identity plan needed for
    // body targets. Sources populate `RawRow.body` directly inside
    // `read_batch`; the Transform interpreter folds it into `computed`,
    // and sinks read both halves through `Row::columns()`.
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
            .field("has_derived", &self.derived.is_some())
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::model::CursorOrder;
    use crate::mapping::ColumnMapping;
    use crate::model::{Field, ReadSpec, Schema, WriteSpec};
    use crate::traits::{MockSink, MockSource, MockStorage};

    fn assembled(rules: Vec<ColumnMapping>) -> AssembledFlow {
        let mut src = MockSource::new();
        src.expect_schemaless().return_const(false);
        src.expect_body_data_type()
            .returning(|| crate::types::DataType::Json);
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        AssembledFlow {
            name: "test".into(),
            source: Arc::new(src),
            sink: Arc::new(sink),
            storage: Arc::new(MockStorage::new()),
            rules,
            read_spec: ReadSpec {
                columns: Vec::new(),
                table: "t".into(),
                cursor_fields: Vec::new(),
                cursor_order: CursorOrder::Asc,
                limit: 1,
                source_options: toml::Table::new(),
                needs_body: false,
            },
            write_spec: WriteSpec {
                columns: Vec::new(),
                table: "t".into(),
                conflict: None,
            },
            interval: Duration::from_millis(10),
            query_timeout: Duration::from_secs(5),
            sampling: SamplingConfig::Disabled,
            access_check: false,
            fields_check: true,
            inserts_check: false,
            cursor_persistence: CursorPersistence::ColumnCursor,
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

    /// `rebuild_derived` populates derived; `invalidate_derived` clears
    /// it; `derived()` errors when cleared.
    #[test]
    fn rebuild_then_invalidate_then_access_errors() {
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
        assert!(state.has_derived());
        state.invalidate_derived();
        assert!(!state.has_derived());
        assert!(state.derived().is_err());

        let src = schema(&[("a", DataType::Int32, false)]);
        let dst = schema(&[("a", DataType::Int32, false)]);
        state.rebuild_derived(&src, &dst).unwrap();
        assert!(state.has_derived());
        let derived = state.derived().unwrap();
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
            fn kind(&self) -> &'static str {
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
}
