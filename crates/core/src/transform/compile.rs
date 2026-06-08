//! Lowering from `ExpandedMapping` + per-column `ColumnConversionPlan`s
//! to the [`Transform`](super::Transform) IR.
//!
//! Pure — no I/O. Lives next to `Transform::apply` so the IR shape and
//! its construction site evolve together.

use std::sync::Arc;

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_runtime::RuntimeProgram;

use crate::error::ValidationError;
use crate::mapping::ExpandedMapping;
use crate::model::{ColumnConversionPlan, Schema};
use crate::transform::{Transform, TransformOp};
use crate::types::{DataType, Value};

/// Per-direct-column lowering decision for a computed column. Built by
/// `flow_state::build_conversions` (which owns the `ExpressionContext` and
/// compiles each script) and consumed here, parallel to
/// `expanded.direct`. `ComputeLowering::None` marks an ordinary
/// `from`-driven / switch column lowered through `conversions` as before.
#[derive(Debug, Clone)]
pub enum ComputeLowering {
    /// Not a compute column — lower via `conversions[i]` as usual.
    None,
    /// A const-folded compute → a literal. `value` is already coerced to
    /// `output` (the sink type) for typed sinks, or the raw constant for
    /// schemaless sinks; `output` is the column's resolved `DataType`.
    Const { value: Value, output: DataType },
    /// An identity compute (`field("x")`) → a plain `Take` of column
    /// `column`, wrapped in `Convert` per `conversions[i]` exactly like a
    /// `from = "x"` direct mapping.
    Identity { column: String },
    /// A general per-row compute. `wrap` is `true` for a typed sink — the
    /// `Compute` op self-coerces its result to `conversions[i].sink`
    /// (honouring truncate / default) — and `false` for a schemaless sink,
    /// where the raw value is written.
    Compute {
        program: Arc<RuntimeProgram>,
        wrap: bool,
    },
}

/// The per-row compute-evaluation context attached to the `Transform`
/// when it has any `Compute` op: the row schema that binds `field("c")`
/// positionally (built in `read_columns` order) plus the function
/// registry used to evaluate the script.
pub type ComputeContext = (Schema, Arc<FunctionRegistry>);

/// Lower an expanded mapping + matching conversion plans into a
/// [`Transform`] program.
///
/// Inputs:
/// - `expanded`: the post-expansion mapping (one `direct` entry per
///   sink direct column, optional `body` block listing the `*:NAME`
///   targets).
/// - `source_body_data_type`: the canonical [`DataType`] the source
///   advertises for its body payload (`Source::body_data_type()`).
///   Must satisfy `is_object()` when the expanded mapping has any body
///   target — non-object bodies cannot be folded.
/// - `conversions`: per-direct-column matrix plans. `conversions.len()`
///   must equal `expanded.direct.len()`.
/// - `body_conversions`: per-body-target matrix plans. Length must equal
///   `expanded.body.as_ref().map(|b| b.targets.len()).unwrap_or(0)`.
/// - `read_columns`: the post-expansion source-side column list
///   (i.e. `expanded.read_columns()`). Used to size the source-index
///   universe for `Take` ops.
/// - `source_schemaless`: when `true`, every non-identity / non-switch
///   direct column emits a dynamic-source [`TransformOp::Convert`]
///   (with `plan.source = None`) keyed off the sink type only — the
///   sampled "source type" is not authoritative for schemaless sources
///   (Mongo). When `false`, the static `Convert` path is emitted with
///   the resolved source `DataType` baked into `plan.source`.
/// - `lowerings`: per-direct-column compute-lowering decisions (parallel
///   to `expanded.direct`). Empty means "all ordinary columns"; a shorter
///   slice treats missing entries as [`ComputeLowering::None`].
/// - `compute_ctx`: the row schema + registry to attach when the program
///   contains any `Compute` op (`None` otherwise).
#[allow(clippy::too_many_arguments)]
pub fn compile_to_transform(
    expanded: &ExpandedMapping,
    source_body_data_type: DataType,
    conversions: &[ColumnConversionPlan],
    body_conversions: &[ColumnConversionPlan],
    read_columns: &[String],
    source_schemaless: bool,
    lowerings: &[ComputeLowering],
    compute_ctx: Option<ComputeContext>,
) -> Result<Transform, ValidationError> {
    if conversions.len() != expanded.direct.len() {
        return Err(invariant(format!(
            "compile_to_transform: conversions.len {} != expanded.direct.len {}",
            conversions.len(),
            expanded.direct.len()
        )));
    }
    let body_target_count = expanded.body.as_ref().map(|b| b.targets.len()).unwrap_or(0);
    if body_conversions.len() != body_target_count {
        return Err(invariant(format!(
            "compile_to_transform: body_conversions.len {} != body targets {}",
            body_conversions.len(),
            body_target_count
        )));
    }
    if body_target_count > 0 && !source_body_data_type.is_object() {
        return Err(invariant(format!(
            "compile_to_transform: source body type {source_body_data_type:?} \
             is not object-shaped — only object types can feed `Body` ops"
        )));
    }

    let read_len = read_columns.len();
    let mut cols: Vec<TransformOp> = Vec::with_capacity(expanded.direct.len() + body_target_count);

    for (i, dm) in expanded.direct.iter().enumerate() {
        let plan = &conversions[i];
        let op = match lowerings.get(i).unwrap_or(&ComputeLowering::None) {
            // Const-folded compute → literal (already coerced for typed
            // sinks). No `Convert`, no source slot.
            ComputeLowering::Const { value, output } => TransformOp::Const {
                value: value.clone(),
                output: output.clone(),
            },
            // Identity compute (`field("x")`) lowers exactly like a direct
            // mapping `from = "x"` — a `Take` of column `x`, wrapped per the
            // conversion plan.
            ComputeLowering::Identity { column } => {
                let source_index = resolve_source_index(column, read_columns)?;
                lower_take(TransformOp::Take { source_index }, plan, source_schemaless)
            }
            // General per-row compute. `needed_indices` are the projection
            // slots the script reads; the op self-coerces to the sink type
            // (typed sink) or writes the raw value (schemaless, `wrap = false`).
            ComputeLowering::Compute { program, wrap } => {
                // A `fields("*")` script reads every projected slot at
                // runtime, so it must register ALL of them as references —
                // otherwise an earlier `Take` of a shared column would be the
                // last reference and move the slot to `Null` before the
                // compute clones it. Named reads (`field`/`fields("a,b")`)
                // register only the columns they touch.
                let needed_indices: Vec<usize> = if program.reads_all_columns() {
                    (0..read_columns.len()).collect()
                } else {
                    let mut indices = Vec::with_capacity(program.needed_columns().len());
                    for name in program.needed_columns() {
                        indices.push(resolve_source_index(name, read_columns)?);
                    }
                    indices
                };
                let (sink, truncate, default) = if *wrap {
                    (
                        Some(plan.sink.clone()),
                        plan.ctx.truncate,
                        plan.ctx.default.clone(),
                    )
                } else {
                    (None, false, None)
                };
                TransformOp::Compute {
                    program: program.clone(),
                    needed_indices,
                    sink,
                    truncate,
                    default,
                }
            }
            // Ordinary `from`-driven / switch column.
            ComputeLowering::None => {
                let source_index = resolve_source_index(&dm.from, read_columns)?;
                lower_take(TransformOp::Take { source_index }, plan, source_schemaless)
            }
        };
        cols.push(op);
    }

    if let Some(body) = &expanded.body {
        for (j, _target) in body.targets.iter().enumerate() {
            let inner = TransformOp::Body;
            let plan = &body_conversions[j];
            let op = if plan.is_identity() {
                inner
            } else {
                TransformOp::Convert {
                    input: Box::new(inner),
                    plan: plan.clone(),
                    truncate: plan.ctx.truncate,
                }
            };
            cols.push(op);
        }
    }

    for op in &cols {
        check_indices_in_range(op, read_len)?;
    }

    let transform = Transform::new(cols, read_columns.to_vec());
    Ok(match compute_ctx {
        Some((read_schema, registry)) => transform.with_compute_context(read_schema, registry),
        None => transform,
    })
}

/// Resolve a source column name to its projection slot in `read_columns`.
fn resolve_source_index(name: &str, read_columns: &[String]) -> Result<usize, ValidationError> {
    read_columns.iter().position(|c| c == name).ok_or_else(|| {
        invariant(format!(
            "compile_to_transform: column {name:?} not found in read_columns"
        ))
    })
}

/// Wrap a `Take` leaf per its conversion plan — the shared lowering for
/// both `from`-driven direct columns and identity compute columns.
fn lower_take(
    take: TransformOp,
    plan: &ColumnConversionPlan,
    source_schemaless: bool,
) -> TransformOp {
    if let Some(switch) = plan.switch.clone() {
        // Switch path: the lookup produces values already in the sink's
        // `DataType`, so no `Convert` wrapping is required. `truncate`
        // travels onto the op for symmetry with the `Convert` arm —
        // `output_type` consults it during `Transform::resolve_types`.
        TransformOp::Switch {
            input: Box::new(take),
            table: switch,
            truncate: plan.ctx.truncate,
        }
    } else if source_schemaless {
        // Schemaless source: the sampled `plan.source` is a hypothesis,
        // not a runtime contract. Even when the hypothesis matches the
        // sink (identity-shape), a single cross-doc shape drift would blow
        // up the static Convert path. We emit a dynamic-source `Convert`
        // (plan.source = None) which dispatches on the actual `Value`
        // variant per cell; the dispatcher short-circuits identity pairs
        // internally, so the cost vs `Take` is one variant probe per cell.
        let mut dyn_plan = plan.clone();
        dyn_plan.source = None;
        TransformOp::Convert {
            input: Box::new(take),
            plan: dyn_plan,
            truncate: plan.ctx.truncate,
        }
    } else if plan.is_identity() {
        take
    } else {
        TransformOp::Convert {
            input: Box::new(take),
            plan: plan.clone(),
            truncate: plan.ctx.truncate,
        }
    }
}

fn check_indices_in_range(op: &TransformOp, read_len: usize) -> Result<(), ValidationError> {
    match op {
        TransformOp::Take { source_index } => {
            if *source_index >= read_len {
                return Err(invariant(format!(
                    "compile_to_transform: Take source_index {source_index} >= read_len {read_len}"
                )));
            }
        }
        TransformOp::Body | TransformOp::Const { .. } => {}
        TransformOp::Compute { needed_indices, .. } => {
            for &i in needed_indices {
                if i >= read_len {
                    return Err(invariant(format!(
                        "compile_to_transform: Compute needed_index {i} >= read_len {read_len}"
                    )));
                }
            }
        }
        TransformOp::Convert { input, .. } | TransformOp::Switch { input, .. } => {
            check_indices_in_range(input, read_len)?
        }
    }
    Ok(())
}

fn invariant(detail: String) -> ValidationError {
    ValidationError::AccessFailed {
        component: "transform:compile",
        name: "<compile>".to_string(),
        source: Box::new(crate::error::RuntimeError::DerivedPlanInvariant { detail }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::mapping::{Body, DirectMapping};
    use crate::model::ColumnConversionPlan;
    use crate::types::DataType;

    fn direct(from: &str, to: &str) -> DirectMapping {
        DirectMapping {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default_literal: None,
            switch: None,
            compute: None,
        }
    }

    #[test]
    fn lowers_pg_to_pg_body() {
        let expanded = ExpandedMapping {
            direct: vec![direct("id", "id")],
            body: Some(Body {
                source_columns: vec!["id".into(), "name".into()],
                targets: vec!["body".into()],
            }),
        };
        let conversions = vec![ColumnConversionPlan::identity(DataType::Int64)];
        let body_conversions = vec![ColumnConversionPlan::identity(DataType::Json)];
        let read_columns = vec!["id".to_string(), "name".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            &conversions,
            &body_conversions,
            &read_columns,
            false,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(t.cols.len(), 2);
        assert!(matches!(&t.cols[0], TransformOp::Take { source_index: 0 }));
        assert!(matches!(&t.cols[1], TransformOp::Body));
    }

    #[test]
    fn wraps_non_identity_conversions_in_convert() {
        let expanded = ExpandedMapping {
            direct: vec![direct("a", "a")],
            body: None,
        };
        let plan = ColumnConversionPlan {
            source: Some(DataType::Int16),
            sink: DataType::Int64,
            ctx: crate::types::ConversionContext::passthrough(),
            switch: None,
        };
        let read_columns = vec!["a".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            false,
            &[],
            None,
        )
        .unwrap();
        match &t.cols[0] {
            TransformOp::Convert { input, .. } => {
                assert!(matches!(
                    input.as_ref(),
                    TransformOp::Take { source_index: 0 }
                ));
            }
            other => panic!("expected Convert, got {other:?}"),
        }
    }

    #[test]
    fn rejects_direct_from_missing_from_read_columns() {
        let expanded = ExpandedMapping {
            direct: vec![direct("missing", "to")],
            body: None,
        };
        let plan = ColumnConversionPlan::identity(DataType::Int32);
        let read_columns: Vec<String> = vec!["other".into()];
        let err = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            false,
            &[],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::AccessFailed { .. }));
    }

    #[test]
    fn rejects_body_conversions_length_mismatch() {
        let expanded = ExpandedMapping {
            direct: Vec::new(),
            body: Some(Body {
                source_columns: Vec::new(),
                targets: vec!["body".into()],
            }),
        };
        let err = compile_to_transform(&expanded, DataType::Json, &[], &[], &[], false, &[], None)
            .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::AccessFailed {
                component: "transform:compile",
                ..
            }
        ));
    }

    #[test]
    fn collapses_identity_plan_convert_to_bare_take() {
        let expanded = ExpandedMapping {
            direct: vec![direct("a", "a")],
            body: None,
        };
        let plan = ColumnConversionPlan {
            source: Some(DataType::Int32),
            sink: DataType::Int32,
            ctx: crate::types::ConversionContext::passthrough(),
            switch: None,
        };
        let read_columns = vec!["a".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            false,
            &[],
            None,
        )
        .unwrap();
        assert!(matches!(&t.cols[0], TransformOp::Take { source_index: 0 }));
    }

    /// `ColumnConversionPlan.switch = Some(table)` lowers to
    /// `TransformOp::Switch { input: Take{i}, table, truncate }` — NOT
    /// wrapped in `Convert`. Catches regressions in the lowering branch
    /// at compile.rs:81 where the switch arm could silently degenerate
    /// into a bare `Take` or a `Convert` wrap.
    #[test]
    fn switch_plan_lowers_to_switch_op() {
        use air_elt_types::Key;

        use crate::transform::switch::SwitchTable;
        use crate::types::Value;

        let mut cases = ahash::AHashMap::new();
        cases.insert(
            Key::single(Value::Bool(true)).unwrap(),
            Value::Text("yes".into()),
        );
        cases.insert(
            Key::single(Value::Bool(false)).unwrap(),
            Value::Text("no".into()),
        );
        let table = SwitchTable {
            cases,
            default: None,
            output_type: DataType::Text { size: None },
        };
        let expanded = ExpandedMapping {
            direct: vec![direct("flag", "label")],
            body: None,
        };
        let plan = ColumnConversionPlan {
            source: Some(DataType::Bool),
            sink: DataType::Text { size: None },
            ctx: crate::types::ConversionContext::passthrough(),
            switch: Some(table),
        };
        let read_columns = vec!["flag".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            false,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(t.cols.len(), 1);
        match &t.cols[0] {
            TransformOp::Switch { input, .. } => {
                assert!(matches!(
                    input.as_ref(),
                    TransformOp::Take { source_index: 0 }
                ));
            }
            other => panic!("expected Switch, got {other:?}"),
        }
    }

    /// Schemaless-both `["*"]` (mongo→mongo) lowers to a single
    /// `Body` op writing the synthetic `_root` target.
    #[test]
    fn root_body_target_lowers_to_single_body() {
        let expanded = ExpandedMapping {
            direct: Vec::new(),
            body: Some(Body {
                source_columns: Vec::new(),
                targets: vec![crate::mapping::ROOT_BODY_TARGET.to_string()],
            }),
        };
        let body_conversions = vec![ColumnConversionPlan::identity(DataType::Json)];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            &[],
            &body_conversions,
            &[],
            false,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(t.cols.len(), 1);
        assert!(matches!(&t.cols[0], TransformOp::Body));
    }
}
