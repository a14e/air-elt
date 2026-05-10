//! Transform IR + interpreter.
//!
//! `Transform` is the program built once per flow during validation that
//! maps a `RawBatch` produced by a source into a final `Batch` ready for
//! a sink. The IR (`TransformOp`) is closed: only the variants needed
//! by today's mapping semantics — extending requires a real consumer
//! per AGENTS.md (no future-proofing of enum variants).
//!
//! Apply optimisation: per row, absorb values from the raw row whenever
//! possible. The LAST reference in op-execution order to a
//! `Take { source_index }` or to a `Body` moves; earlier references
//! clone. The "last reference" map is precomputed once per
//! `Transform::new` so apply is a tight `for op in cols` loop.

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::ColumnConversionPlan;
use crate::model::raw::{RawBatch, RawRow};
use crate::model::{Batch, Row};
use crate::types::Value;
use crate::types::convert::convert;

/// IR for the per-flow Transform program. Each variant maps a chunk of
/// `RawRow` into one sink output column.
///
/// The variant set is closed: `Take`, `Body`, `Convert`. Extending
/// requires a real consumer; do NOT add hypothetical variants.
#[derive(Clone, Debug)]
pub enum TransformOp {
    /// Move (or clone, if not the last reference) `raw.values[source_index]`
    /// into the sink slot.
    Take { source_index: usize },
    /// Move (or clone, if not the last reference) `raw.body` into the
    /// sink slot. Sources push the body as `Value::Json(...)` (relational)
    /// or `Value::Custom(BsonObjectValue(...))` (mongo); the compile
    /// step asserts the source's `body_data_type().is_object()`.
    Body,
    /// Wrap any other op and post-convert through the matrix.
    Convert {
        input: Box<TransformOp>,
        plan: ColumnConversionPlan,
    },
}

/// Compiled Transform program. One op per sink output column.
#[derive(Clone, Debug)]
pub struct Transform {
    pub cols: Vec<TransformOp>,
    /// `last_take_for[i] = Some(col_idx)` means: the last col that
    /// references `Take { source_index = i }` — directly or recursively
    /// inside a `Convert` — sits at `cols[col_idx]`. That op gets to
    /// move the value out of `raw.values[i]`; earlier references clone.
    /// `None` means no op references source index `i`.
    pub(crate) last_take_for: Vec<Option<usize>>,
    /// Index of the last col that references a `Body` (directly or
    /// recursively inside a `Convert`). `None` when no `Body` op
    /// appears.
    pub(crate) last_body: Option<usize>,
    /// Cached at construction: every col is `Take { source_index = i }`
    /// for `i in 0..cols.len()`. Apply uses this to skip per-column work
    /// and forward `raw.values` straight into `Row.values`.
    is_identity: bool,
}

impl Transform {
    /// Build a Transform program. Precomputes the last-reference maps
    /// used by `apply` to absorb values from the raw row whenever
    /// possible.
    pub fn new(cols: Vec<TransformOp>) -> Self {
        // Single pass: each op walks straight to its leaf (`Take` or
        // `Body`) by peeling off any `Convert` wrappers, and records
        // whichever last-reference map applies. `last_take_for` grows
        // lazily as new source indices appear.
        let mut last_take_for: Vec<Option<usize>> = Vec::new();
        let mut last_body: Option<usize> = None;
        for (col_idx, op) in cols.iter().enumerate() {
            let mut current = op;
            loop {
                match current {
                    TransformOp::Take { source_index } => {
                        let i = *source_index;
                        if i >= last_take_for.len() {
                            last_take_for.resize(i + 1, None);
                        }
                        last_take_for[i] = Some(col_idx);
                        break;
                    }
                    TransformOp::Body => {
                        last_body = Some(col_idx);
                        break;
                    }
                    TransformOp::Convert { input, .. } => current = input,
                }
            }
        }
        let is_identity = cols
            .iter()
            .enumerate()
            .all(|(i, op)| matches!(op, TransformOp::Take { source_index } if *source_index == i));
        Self {
            cols,
            last_take_for,
            last_body,
            is_identity,
        }
    }

    /// Identity short-circuit: every col is `Take { source_index = i }`
    /// for `i in 0..cols.len()`. Computed once in [`Self::new`].
    pub fn is_identity(&self) -> bool {
        self.is_identity
    }

    /// Run the program over a `RawBatch`, producing a `Batch` shaped
    /// for the sink.
    pub fn apply(&self, raw: RawBatch) -> RuntimeResult<Batch> {
        let RawBatch { rows, next_cursor } = raw;
        if self.is_identity {
            let out_rows = rows
                .into_iter()
                .map(|r| Row {
                    values: r.values,
                    op: r.op,
                })
                .collect();
            return Ok(Batch {
                rows: out_rows,
                next_cursor,
            });
        }
        let mut out_rows: Vec<Row> = Vec::with_capacity(rows.len());
        for raw_row in rows {
            let RawRow {
                mut values,
                mut body,
                op,
            } = raw_row;
            let out_values: Vec<Value> = self
                .cols
                .iter()
                .enumerate()
                .map(|(col_idx, c)| self.eval_op(c, col_idx, &mut values, &mut body))
                .collect::<RuntimeResult<_>>()?;
            out_rows.push(Row {
                values: out_values,
                op,
            });
        }
        Ok(Batch {
            rows: out_rows,
            next_cursor,
        })
    }

    pub(crate) fn eval_op(
        &self,
        op: &TransformOp,
        col_idx: usize,
        values: &mut Vec<Value>,
        body: &mut Option<Value>,
    ) -> RuntimeResult<Value> {
        match op {
            TransformOp::Take { source_index } => {
                let i = *source_index;
                if i >= values.len() {
                    return Err(RuntimeError::DerivedPlanInvariant {
                        detail: format!(
                            "Transform::Take source_index {i} out of bounds (raw values len {})",
                            values.len()
                        ),
                    });
                }
                if self.last_take_for.get(i).copied().flatten() == Some(col_idx) {
                    Ok(std::mem::replace(&mut values[i], Value::Null))
                } else {
                    Ok(values[i].clone())
                }
            }
            TransformOp::Body => {
                let is_last = self.last_body == Some(col_idx);
                if is_last {
                    body.take()
                        .ok_or_else(|| RuntimeError::DerivedPlanInvariant {
                            detail: "Transform::Body: raw_row.body is None — \
                                 source must attach a body when needs_body=true"
                                .to_string(),
                        })
                } else {
                    body.as_ref()
                        .cloned()
                        .ok_or_else(|| RuntimeError::DerivedPlanInvariant {
                            detail: "Transform::Body: raw_row.body is None — \
                                     source must attach a body when needs_body=true"
                                .to_string(),
                        })
                }
            }
            TransformOp::Convert { input, plan } => {
                let v = self.eval_op(input, col_idx, values, body)?;
                let out = convert(v, &plan.source, &plan.sink, &plan.ctx)?;
                Ok(out)
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::RowOp;
    use crate::types::ConversionContext;
    use crate::types::data_type::DataType;
    use crate::types::value::Value;

    fn raw_row(values: Vec<Value>) -> RawRow {
        RawRow {
            values,
            body: None,
            op: RowOp::Upsert,
        }
    }

    fn raw_row_with_body(values: Vec<Value>, body: Value) -> RawRow {
        RawRow {
            values,
            body: Some(body),
            op: RowOp::Upsert,
        }
    }

    fn batch_of(rows: Vec<RawRow>) -> RawBatch {
        RawBatch {
            rows,
            next_cursor: None,
        }
    }

    #[test]
    fn identity_returns_byte_identical_values() {
        let t = Transform::new(vec![
            TransformOp::Take { source_index: 0 },
            TransformOp::Take { source_index: 1 },
            TransformOp::Take { source_index: 2 },
        ]);
        assert!(t.is_identity());
        let raw = batch_of(vec![raw_row(vec![
            Value::Int32(10),
            Value::Text("a".into()),
            Value::Int64(99),
        ])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows.len(), 1);
        assert_eq!(
            batch.rows[0].values,
            vec![Value::Int32(10), Value::Text("a".into()), Value::Int64(99)]
        );
        assert_eq!(batch.rows[0].op, RowOp::Upsert);
    }

    #[test]
    fn reorder_via_take() {
        let t = Transform::new(vec![
            TransformOp::Take { source_index: 2 },
            TransformOp::Take { source_index: 0 },
            TransformOp::Take { source_index: 1 },
        ]);
        assert!(!t.is_identity());
        let raw = batch_of(vec![raw_row(vec![
            Value::Int32(1),
            Value::Int32(2),
            Value::Int32(3),
        ])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(
            batch.rows[0].values,
            vec![Value::Int32(3), Value::Int32(1), Value::Int32(2)]
        );
    }

    #[test]
    fn body_move_consumes_payload() {
        let t = Transform::new(vec![TransformOp::Body]);
        let payload = serde_json::json!({"k":1});
        let raw = batch_of(vec![raw_row_with_body(
            vec![],
            Value::Json(payload.clone()),
        )]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Json(payload)]);
    }

    #[test]
    fn body_invariant_when_payload_missing() {
        let t = Transform::new(vec![TransformOp::Body]);
        let raw = batch_of(vec![raw_row(vec![])]);
        let err = t.apply(raw).unwrap_err();
        assert!(matches!(err, RuntimeError::DerivedPlanInvariant { .. }));
    }

    #[test]
    fn convert_wraps_take_int16_to_int64() {
        let plan = ColumnConversionPlan {
            source: DataType::Int16,
            sink: DataType::Int64,
            ctx: ConversionContext::passthrough(),
        };
        let t = Transform::new(vec![TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan,
        }]);
        let raw = batch_of(vec![raw_row(vec![Value::Int16(42)])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Int64(42)]);
    }

    #[test]
    fn convert_wraps_body_into_text() {
        let plan = ColumnConversionPlan {
            source: DataType::Json,
            sink: DataType::Text { size: None },
            ctx: ConversionContext::passthrough(),
        };
        let t = Transform::new(vec![TransformOp::Convert {
            input: Box::new(TransformOp::Body),
            plan,
        }]);
        let raw = batch_of(vec![raw_row_with_body(
            vec![],
            Value::Json(serde_json::json!({"k":7})),
        )]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Text("{\"k\":7}".into())]);
    }

    #[test]
    fn is_identity_table() {
        let t = Transform::new(vec![
            TransformOp::Take { source_index: 0 },
            TransformOp::Take { source_index: 1 },
        ]);
        assert!(t.is_identity());

        let t = Transform::new(vec![
            TransformOp::Take { source_index: 1 },
            TransformOp::Take { source_index: 0 },
        ]);
        assert!(!t.is_identity());

        let t = Transform::new(vec![]);
        assert!(t.is_identity());

        let t = Transform::new(vec![TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan: ColumnConversionPlan {
                source: DataType::Int16,
                sink: DataType::Int64,
                ctx: ConversionContext::passthrough(),
            },
        }]);
        assert!(!t.is_identity());

        let t = Transform::new(vec![TransformOp::Body]);
        assert!(!t.is_identity());
    }

    #[test]
    fn last_take_absorb_when_last_two_refs_same_index() {
        let t = Transform::new(vec![
            TransformOp::Take { source_index: 0 },
            TransformOp::Take { source_index: 0 },
        ]);
        assert_eq!(t.last_take_for, vec![Some(1)]);

        let RawRow {
            mut values,
            mut body,
            ..
        } = raw_row(vec![Value::Int32(123)]);
        let v0 = t
            .eval_op(&t.cols[0], 0, &mut values, &mut body)
            .expect("first eval clones");
        assert_eq!(v0, Value::Int32(123));
        assert_eq!(values[0], Value::Int32(123));

        let v1 = t
            .eval_op(&t.cols[1], 1, &mut values, &mut body)
            .expect("second eval moves");
        assert_eq!(v1, Value::Int32(123));
        assert_eq!(values[0], Value::Null);
    }

    #[test]
    fn last_body_absorb_when_last_two_body_ops() {
        let t = Transform::new(vec![TransformOp::Body, TransformOp::Body]);
        assert_eq!(t.last_body, Some(1));

        let RawRow {
            mut values,
            mut body,
            ..
        } = raw_row_with_body(vec![], Value::Json(serde_json::json!({"k":1})));
        let v0 = t
            .eval_op(&t.cols[0], 0, &mut values, &mut body)
            .expect("first body clones");
        assert_eq!(v0, Value::Json(serde_json::json!({"k":1})));
        assert!(body.is_some());

        let v1 = t
            .eval_op(&t.cols[1], 1, &mut values, &mut body)
            .expect("second body moves");
        assert_eq!(v1, Value::Json(serde_json::json!({"k":1})));
        assert!(body.is_none());
    }

    /// Lowering smoke test: a pg-shaped body flow lowers to one `Take`
    /// (for `id`) + one `Body` op, and `Transform::apply` forwards the
    /// body the source attached.
    #[test]
    fn transform_lowering_pg_body() {
        use crate::mapping::{Body, DirectMapping, ExpandedMapping};
        use crate::model::ColumnConversionPlan;
        use crate::transform::compile_to_transform;

        let expanded = ExpandedMapping {
            direct: vec![DirectMapping {
                from: "id".into(),
                to: "id".into(),
                truncate: false,
                default_literal: None,
            }],
            body: Some(Body {
                source_columns: vec!["id".into(), "name".into()],
                targets: vec!["body".into()],
            }),
        };
        let conversions = vec![ColumnConversionPlan::identity(DataType::Int64)];
        let body_conversions = vec![ColumnConversionPlan::identity(DataType::Json)];
        let read_columns: Vec<String> = vec!["id".into(), "name".into()];

        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            &conversions,
            &body_conversions,
            &read_columns,
        )
        .unwrap();

        let raw = batch_of(vec![raw_row_with_body(
            vec![Value::Int64(7), Value::Text("alice".into())],
            Value::Json(serde_json::json!({"id": 7, "name": "alice"})),
        )]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows.len(), 1);
        assert_eq!(
            batch.rows[0].values,
            vec![
                Value::Int64(7),
                Value::Json(serde_json::json!({"id": 7, "name": "alice"})),
            ]
        );
    }

    #[test]
    fn last_take_absorb_through_convert() {
        let plan = ColumnConversionPlan {
            source: DataType::Int32,
            sink: DataType::Int32,
            ctx: ConversionContext::passthrough(),
        };
        let t = Transform::new(vec![
            TransformOp::Take { source_index: 0 },
            TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan,
            },
        ]);
        assert_eq!(t.last_take_for, vec![Some(1)]);

        let RawRow {
            mut values,
            mut body,
            ..
        } = raw_row(vec![Value::Int32(7)]);
        let v0 = t.eval_op(&t.cols[0], 0, &mut values, &mut body).unwrap();
        assert_eq!(v0, Value::Int32(7));
        assert_eq!(values[0], Value::Int32(7));

        let v1 = t.eval_op(&t.cols[1], 1, &mut values, &mut body).unwrap();
        assert_eq!(v1, Value::Int32(7));
        assert_eq!(values[0], Value::Null);
    }

    #[test]
    fn last_body_absorb_through_convert() {
        let plan = ColumnConversionPlan {
            source: DataType::Json,
            sink: DataType::Json,
            ctx: ConversionContext::passthrough(),
        };
        let t = Transform::new(vec![
            TransformOp::Body,
            TransformOp::Convert {
                input: Box::new(TransformOp::Body),
                plan,
            },
        ]);
        assert_eq!(t.last_body, Some(1));

        let RawRow {
            mut values,
            mut body,
            ..
        } = raw_row_with_body(vec![], Value::Json(serde_json::json!({"k":1})));
        let _ = t.eval_op(&t.cols[0], 0, &mut values, &mut body).unwrap();
        assert!(body.is_some());
        let _ = t.eval_op(&t.cols[1], 1, &mut values, &mut body).unwrap();
        assert!(body.is_none());
    }

    #[test]
    fn transform_lowering_rejects_non_object_body_type() {
        use crate::mapping::{Body, ExpandedMapping};
        use crate::model::ColumnConversionPlan;
        use crate::transform::compile_to_transform;

        let expanded = ExpandedMapping {
            direct: Vec::new(),
            body: Some(Body {
                source_columns: vec!["a".into()],
                targets: vec!["body".into()],
            }),
        };
        let body_conversions = vec![ColumnConversionPlan::identity(DataType::Int32)];
        let read_columns: Vec<String> = Vec::new();
        let res = compile_to_transform(
            &expanded,
            DataType::Int32,
            &[],
            &body_conversions,
            &read_columns,
        );
        assert!(res.is_err(), "non-object source body must error");
    }
}
