//! Lowering from `ExpandedMapping` + per-column `ColumnConversionPlan`s
//! to the [`Transform`](super::Transform) IR.
//!
//! Pure — no I/O. Lives next to `Transform::apply` so the IR shape and
//! its construction site evolve together.

use crate::error::ValidationError;
use crate::mapping::ExpandedMapping;
use crate::model::ColumnConversionPlan;
use crate::transform::{Transform, TransformOp};
use crate::types::DataType;

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
pub fn compile_to_transform(
    expanded: &ExpandedMapping,
    source_body_data_type: DataType,
    conversions: &[ColumnConversionPlan],
    body_conversions: &[ColumnConversionPlan],
    read_columns: &[String],
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
        let source_index = read_columns
            .iter()
            .position(|c| c == &dm.from)
            .ok_or_else(|| {
                invariant(format!(
                    "compile_to_transform: direct.from {:?} not found in read_columns",
                    dm.from
                ))
            })?;
        if source_index >= read_len {
            return Err(invariant(format!(
                "compile_to_transform: direct source_index {source_index} out of range \
                 (read_columns.len {read_len})"
            )));
        }
        let take = TransformOp::Take { source_index };
        let plan = &conversions[i];
        let op = if let Some(switch) = plan.switch.clone() {
            // Switch path: the lookup produces values already in the
            // sink's `DataType`, so no `Convert` wrapping is required.
            // `truncate` travels onto the op for symmetry with the
            // `Convert` arm — `output_type` consults it during
            // `Transform::resolve_types`.
            TransformOp::Switch {
                input: Box::new(take),
                table: switch,
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

    Ok(Transform::new(cols, read_columns.to_vec()))
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
        TransformOp::Body => {}
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
            source: DataType::Int16,
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
        let err = compile_to_transform(&expanded, DataType::Json, &[], &[], &[]).unwrap_err();
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
            source: DataType::Int32,
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
        use crate::transform::switch::{SwitchKey, SwitchTable};
        use crate::types::Value;

        let mut cases = ahash::AHashMap::new();
        cases.insert(SwitchKey::Bool(true), Value::Text("yes".into()));
        cases.insert(SwitchKey::Bool(false), Value::Text("no".into()));
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
            source: DataType::Bool,
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
        let t =
            compile_to_transform(&expanded, DataType::Json, &[], &body_conversions, &[]).unwrap();
        assert_eq!(t.cols.len(), 1);
        assert!(matches!(&t.cols[0], TransformOp::Body));
    }
}
