//! Transform IR + interpreter.
//!
//! `Transform` is the program built once per flow during validation that
//! maps a `Batch` produced by a source into a final `Batch` ready for
//! a sink. The IR (`TransformOp`) is closed: only the variants needed
//! by today's mapping semantics — extending requires a real consumer
//! per AGENTS.md (no future-proofing of enum variants).
//!
//! Apply optimisation: per row, absorb values from the raw row whenever
//! possible. The LAST reference in op-execution order to a
//! `Take { source_index }` or to a `Body` moves; earlier references
//! clone. The "last reference" map is precomputed once per
//! `Transform::new` so apply is a tight `for op in cols` loop.

use crate::error::{RuntimeError, RuntimeResult, ValidationError};
use crate::model::ColumnConversionPlan;
use crate::model::{Batch, Row, Schema};
use crate::transform::switch::{SwitchKey, SwitchTable};
use crate::types::Value;
use crate::types::convert::convert;
use crate::types::data_type::DataType;

/// IR for the per-flow Transform program. Each variant maps a chunk of
/// `Row` into one sink output column.
///
/// The variant set is closed: `Take`, `Body`, `Convert`, `Switch`.
/// Extending requires a real consumer; do NOT add hypothetical variants.
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
    /// Wrap any other op and post-convert through the matrix. The
    /// `truncate` flag mirrors `ColumnConversionPlan.ctx.truncate` so
    /// `output_type` can consult the right compatibility relation
    /// (`is_compatible_with_truncate` when on).
    ///
    /// The plan's `source` field selects the dispatch mode:
    ///
    /// * `Some(t)` — static fast path. The source `DataType` is known
    ///   at compile time (typed source like Postgres/MySQL whose
    ///   `information_schema` is authoritative).
    /// * `None` — dynamic dispatch. The source `DataType` is resolved
    ///   per cell from the actual `Value` variant via
    ///   [`Value::data_type`]. Emitted for schemaless sources (Mongo)
    ///   whose sampled "source type" is not authoritative — baking it
    ///   into a static plan would blow up on legitimate cross-doc
    ///   shape drift (an `Int32` sample followed by an `Int64`
    ///   document).
    Convert {
        input: Box<TransformOp>,
        plan: ColumnConversionPlan,
        truncate: bool,
    },
    /// Value-to-value lookup. Evaluates `input` to a source value,
    /// hashes it through [`SwitchKey::from_value`], and either returns
    /// the matched value or `table.default` on miss / NULL input. The
    /// `truncate` flag is opaque to the runtime — it travels with the
    /// op so the same compatibility opt-in is visible alongside the
    /// `Convert` arm; compile-time RHS shortening is already baked
    /// into `table.cases`.
    Switch {
        input: Box<TransformOp>,
        table: SwitchTable,
        truncate: bool,
    },
}

impl TransformOp {
    /// Output `DataType` for this op given the resolved leaf input type.
    /// Leaves (`Take` / `Body`) pass the input through — the caller (in
    /// practice [`Transform::resolve_types`]) is responsible for
    /// resolving each leaf to its concrete source type before invoking
    /// `output_type` on the enclosing op chain.
    ///
    /// `Convert` always succeeds with `plan.sink` — the sink-vs-source
    /// compatibility gate lives in
    /// [`crate::validation::compatibility::CompatibilityValidator`].
    /// `Switch` returns `SwitchUnsupportedSource` when fed a
    /// non-switchable input shape — surfaces at validate time via
    /// `resolve_types`. `Take` / `Body` pass the input through
    /// unchanged.
    pub fn output_type(&self, input: &DataType) -> Result<DataType, ValidationError> {
        match self {
            TransformOp::Take { .. } | TransformOp::Body => Ok(input.clone()),
            // Convert promotes input to `plan.sink` unconditionally; the
            // sink-vs-source compatibility check lives in
            // `core::validation::compatibility::CompatibilityValidator`
            // (one canonical gate, not two). Both lossless and
            // truncate-augmented relations are admitted through the
            // matrix that fits the column's declared `truncate` flag —
            // gating here would be a stricter duplicate and would
            // reject pairs the matrix considers valid via
            // domain-specific conversion arms (e.g. `Int → Text`,
            // `Bool → Text`, etc.).
            TransformOp::Convert { plan, .. } => Ok(plan.sink.clone()),
            TransformOp::Switch { table, .. } => {
                if !super::switch::is_switchable_source(input) {
                    return Err(ValidationError::SwitchUnsupportedSource {
                        flow: "<transform>".into(),
                        column: "<transform>".into(),
                        source_type: input.clone(),
                    });
                }
                Ok(table.output_type.clone())
            }
        }
    }

    /// Whether this op opts into the truncate-augmented matrix when its
    /// output is checked against a sink column. Leaves (`Take` / `Body`)
    /// have no declarative truncate — they pass their input through
    /// unchanged and would represent a config error if their source
    /// type were narrower than the sink. `Convert` and `Switch` carry
    /// the mapping's `truncate` flag verbatim.
    pub fn truncate_flag(&self) -> bool {
        match self {
            TransformOp::Take { .. } | TransformOp::Body => false,
            TransformOp::Convert { truncate, .. } | TransformOp::Switch { truncate, .. } => {
                *truncate
            }
        }
    }
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
    /// Source-side projection list — a copy of `ReadSpec.columns`. Used
    /// by `resolve_types` to look up each `Take.source_index` by name
    /// (`source_schema.find(&self.read_columns[i])`). Schemas may
    /// report fields in a different order than the projection, so the
    /// index domain of `Take` (= projection slot) and the index domain
    /// of `source_schema.fields()` (= schema natural order) are
    /// unrelated.
    pub(crate) read_columns: Vec<String>,
}

impl Transform {
    /// Build a Transform program. Precomputes the last-reference maps
    /// used by `apply` to absorb values from the raw row whenever
    /// possible. `read_columns` is the source-side projection list (the
    /// same `ReadSpec.columns` the source emits values for); used by
    /// `resolve_types` for name-based leaf lookup.
    pub fn new(cols: Vec<TransformOp>, read_columns: Vec<String>) -> Self {
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
                    TransformOp::Convert { input, .. } | TransformOp::Switch { input, .. } => {
                        current = input
                    }
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
            read_columns,
        }
    }

    /// Identity short-circuit: every col is `Take { source_index = i }`
    /// for `i in 0..cols.len()`. Computed once in [`Self::new`].
    pub fn is_identity(&self) -> bool {
        self.is_identity
    }

    /// Run the program over a source-produced `Batch`, returning a
    /// sink-shaped `Batch`. The identity short-circuit returns the
    /// input batch unchanged when every row's `body` is `None`,
    /// avoiding any per-row reconstruction.
    pub fn apply(&self, batch: Batch) -> RuntimeResult<Batch> {
        if self.is_identity && batch.rows.iter().all(|r| r.body.is_none()) {
            return Ok(batch);
        }
        let Batch { rows, next_cursor } = batch;
        let mut out_rows: Vec<Row> = Vec::with_capacity(rows.len());
        for raw_row in rows {
            let Row {
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
                body: None,
                op,
            });
        }
        Ok(Batch {
            rows: out_rows,
            next_cursor,
        })
    }

    /// Walk every col and return its post-transform output `DataType`.
    /// Each leaf (`Take { i }` / `Body`) resolves against the source
    /// schema or the source body type; enclosing ops apply
    /// [`TransformOp::output_type`] on the way back up.
    ///
    /// Returned vector aligns 1:1 with `self.cols`. Consumed by
    /// `validation::compatibility::CompatibilityValidator` to compare
    /// each transformed column against its sink slot — that comparison
    /// closes the cross-family rejection gap (`Bool → Text` via
    /// `Switch` was previously refused by the source-vs-sink matrix
    /// check; here we compare `Switch.output_type` vs sink instead).
    /// Resolves each leaf `Take { source_index = i }` by **name** via
    /// `source_schema.find(&self.read_columns[i])`. We do not index
    /// `source_schema.fields()` positionally because the projection
    /// order (`read_columns`) is independent of the schema's natural
    /// field order — Mongo's sample inferrer iterates an `AHashMap`;
    /// SQL `information_schema` orders by `ordinal_position`; TOML
    /// mapping keys arrive alphabetically. The schema is a name→type
    /// dictionary, the projection is what defines the per-row slot
    /// order.
    pub fn resolve_types(
        &self,
        source_schema: &Schema,
        source_body_type: &DataType,
    ) -> Result<Vec<DataType>, ValidationError> {
        self.cols
            .iter()
            .map(|op| resolve_op(op, source_schema, source_body_type, &self.read_columns))
            .collect()
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
            TransformOp::Convert { input, plan, .. } => {
                let v = self.eval_op(input, col_idx, values, body)?;
                // `plan.source = Some(t)`: static fast path — the source
                // `DataType` is known at compile time (typed source).
                // `plan.source = None`: dynamic dispatch — resolve the
                // source `DataType` from the actual `Value` variant per
                // cell. Null inputs short-circuit via the dispatcher's
                // own null/default arm — `data_type()` returns `None` on
                // null but `convert` treats `Null` before inspecting
                // `src`, so any placeholder is fine for the null case.
                let src = match &plan.source {
                    Some(t) => t.clone(),
                    None => v.data_type().unwrap_or_else(|| plan.sink.clone()),
                };
                let out = convert(v, &src, &plan.sink, &plan.ctx)?;
                Ok(out)
            }
            TransformOp::Switch { input, table, .. } => {
                let v = self.eval_op(input, col_idx, values, body)?;
                if matches!(v, Value::Null) {
                    return Ok(table.default.as_ref().cloned().unwrap_or(Value::Null));
                }
                let Some(key) = SwitchKey::from_value(&v) else {
                    return Err(RuntimeError::DerivedPlanInvariant {
                        detail: format!(
                            "Transform::Switch: source value {v:?} cannot produce a canonical \
                             switch key — validation should have rejected this flow"
                        ),
                    });
                };
                let out = table
                    .cases
                    .get(&key)
                    .cloned()
                    .or_else(|| table.default.as_ref().cloned())
                    .unwrap_or(Value::Null);
                Ok(out)
            }
        }
    }
}

/// Recursive helper for [`Transform::resolve_types`]. Walks down through
/// `Convert` / `Switch` wrappers to the leaf, resolves the leaf to a
/// concrete `DataType` against the source schema or source body type,
/// and applies `output_type` on the way back up.
fn resolve_op(
    op: &TransformOp,
    source_schema: &Schema,
    source_body_type: &DataType,
    read_columns: &[String],
) -> Result<DataType, ValidationError> {
    match op {
        TransformOp::Take { source_index } => {
            let name =
                read_columns
                    .get(*source_index)
                    .ok_or_else(|| ValidationError::AccessFailed {
                        component: "transform:resolve",
                        name: "<transform>".into(),
                        source: Box::new(RuntimeError::DerivedPlanInvariant {
                            detail: format!(
                                "Transform::Take source_index {} out of range \
                                 (read_columns len {})",
                                source_index,
                                read_columns.len()
                            ),
                        }),
                    })?;
            let field = source_schema
                .find(name)
                .ok_or_else(|| ValidationError::MissingField {
                    side: "source",
                    field: name.clone(),
                })?;
            Ok(field.data_type.clone())
        }
        TransformOp::Body => Ok(source_body_type.clone()),
        // Dynamic-source `Convert` (plan.source = None) short-circuits at
        // the resolver: the source type is per-cell (unknown at
        // validation time) and the op's `output_type` returns `plan.sink`
        // directly. We deliberately skip recursing into `input` so a
        // missing source field (schemaless source with no sample / sample
        // missing this column) does not surface as `MissingField` here —
        // schemaless source schemas are non-authoritative by contract.
        TransformOp::Convert { plan, .. } if plan.source.is_none() => Ok(plan.sink.clone()),
        TransformOp::Convert { input, .. } | TransformOp::Switch { input, .. } => {
            let inner = resolve_op(input, source_schema, source_body_type, read_columns)?;
            op.output_type(&inner)
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

    fn raw_row(values: Vec<Value>) -> Row {
        Row {
            values,
            body: None,
            op: RowOp::Upsert,
        }
    }

    fn raw_row_with_body(values: Vec<Value>, body: Value) -> Row {
        Row {
            values,
            body: Some(body),
            op: RowOp::Upsert,
        }
    }

    fn batch_of(rows: Vec<Row>) -> Batch {
        Batch {
            rows,
            next_cursor: None,
        }
    }

    #[test]
    fn identity_returns_byte_identical_values() {
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 0 },
                TransformOp::Take { source_index: 1 },
                TransformOp::Take { source_index: 2 },
            ],
            Vec::new(),
        );
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
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 2 },
                TransformOp::Take { source_index: 0 },
                TransformOp::Take { source_index: 1 },
            ],
            Vec::new(),
        );
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
        let t = Transform::new(vec![TransformOp::Body], Vec::new());
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
        let t = Transform::new(vec![TransformOp::Body], Vec::new());
        let raw = batch_of(vec![raw_row(vec![])]);
        let err = t.apply(raw).unwrap_err();
        assert!(matches!(err, RuntimeError::DerivedPlanInvariant { .. }));
    }

    #[test]
    fn convert_wraps_take_int16_to_int64() {
        let plan = ColumnConversionPlan {
            source: Some(DataType::Int16),
            sink: DataType::Int64,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let t = Transform::new(
            vec![TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Int16(42)])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Int64(42)]);
    }

    #[test]
    fn convert_wraps_body_into_text() {
        let plan = ColumnConversionPlan {
            source: Some(DataType::Json),
            sink: DataType::Text { size: None },
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let t = Transform::new(
            vec![TransformOp::Convert {
                input: Box::new(TransformOp::Body),
                plan,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row_with_body(
            vec![],
            Value::Json(serde_json::json!({"k":7})),
        )]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Text("{\"k\":7}".into())]);
    }

    #[test]
    fn is_identity_table() {
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 0 },
                TransformOp::Take { source_index: 1 },
            ],
            Vec::new(),
        );
        assert!(t.is_identity());

        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 1 },
                TransformOp::Take { source_index: 0 },
            ],
            Vec::new(),
        );
        assert!(!t.is_identity());

        let t = Transform::new(vec![], Vec::new());
        assert!(t.is_identity());

        let t = Transform::new(
            vec![TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan: ColumnConversionPlan {
                    source: Some(DataType::Int16),
                    sink: DataType::Int64,
                    ctx: ConversionContext::passthrough(),
                    switch: None,
                },
                truncate: false,
            }],
            Vec::new(),
        );
        assert!(!t.is_identity());

        let t = Transform::new(vec![TransformOp::Body], Vec::new());
        assert!(!t.is_identity());
    }

    #[test]
    fn last_take_absorb_when_last_two_refs_same_index() {
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 0 },
                TransformOp::Take { source_index: 0 },
            ],
            Vec::new(),
        );
        assert_eq!(t.last_take_for, vec![Some(1)]);

        let Row {
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
        let t = Transform::new(vec![TransformOp::Body, TransformOp::Body], Vec::new());
        assert_eq!(t.last_body, Some(1));

        let Row {
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
                switch: None,
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
            false,
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
            source: Some(DataType::Int32),
            sink: DataType::Int32,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 0 },
                TransformOp::Convert {
                    input: Box::new(TransformOp::Take { source_index: 0 }),
                    plan,
                    truncate: false,
                },
            ],
            Vec::new(),
        );
        assert_eq!(t.last_take_for, vec![Some(1)]);

        let Row {
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
            source: Some(DataType::Json),
            sink: DataType::Json,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let t = Transform::new(
            vec![
                TransformOp::Body,
                TransformOp::Convert {
                    input: Box::new(TransformOp::Body),
                    plan,
                    truncate: false,
                },
            ],
            Vec::new(),
        );
        assert_eq!(t.last_body, Some(1));

        let Row {
            mut values,
            mut body,
            ..
        } = raw_row_with_body(vec![], Value::Json(serde_json::json!({"k":1})));
        let _ = t.eval_op(&t.cols[0], 0, &mut values, &mut body).unwrap();
        assert!(body.is_some());
        let _ = t.eval_op(&t.cols[1], 1, &mut values, &mut body).unwrap();
        assert!(body.is_none());
    }

    fn switch_table_with<I>(cases: I, default: Option<Value>) -> SwitchTable
    where
        I: IntoIterator<Item = (SwitchKey, Value)>,
    {
        let mut m = ahash::AHashMap::new();
        for (k, v) in cases {
            m.insert(k, v);
        }
        // Tests below feed `Text` RHS values; `Text { size: None }` is
        // the matching post-switch output type. `output_type` is only
        // consumed by `resolve_types`, not by `apply`, so the runtime
        // tests are insensitive to the exact choice.
        SwitchTable {
            cases: m,
            default,
            output_type: DataType::Text { size: None },
        }
    }

    #[test]
    fn switch_hit_returns_case_value() {
        let table = switch_table_with(
            [
                (
                    SwitchKey::Text("ACTIVE".into()),
                    Value::Text("active".into()),
                ),
                (
                    SwitchKey::Text("FINISHED".into()),
                    Value::Text("finished".into()),
                ),
            ],
            Some(Value::Text("unknown".into())),
        );
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Text("ACTIVE".into())])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Text("active".into())]);
    }

    #[test]
    fn switch_miss_returns_default() {
        let table = switch_table_with(
            [(
                SwitchKey::Text("ACTIVE".into()),
                Value::Text("active".into()),
            )],
            Some(Value::Text("unknown".into())),
        );
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Text("OTHER".into())])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Text("unknown".into())]);
    }

    #[test]
    fn switch_miss_without_default_returns_null() {
        let table = switch_table_with(
            [(
                SwitchKey::Text("ACTIVE".into()),
                Value::Text("active".into()),
            )],
            None,
        );
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Text("OTHER".into())])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Null]);
    }

    #[test]
    fn switch_null_source_returns_default() {
        let table = switch_table_with(
            [(
                SwitchKey::Text("ACTIVE".into()),
                Value::Text("active".into()),
            )],
            Some(Value::Text("unknown".into())),
        );
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Null])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Text("unknown".into())]);
    }

    #[test]
    fn switch_null_source_without_default_returns_null() {
        let table = switch_table_with(
            [(
                SwitchKey::Text("ACTIVE".into()),
                Value::Text("active".into()),
            )],
            None,
        );
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Null])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Null]);
    }

    /// The "absorb-when-last" optimisation must reach inside
    /// `TransformOp::Switch` exactly as it does for `TransformOp::Convert`
    /// — `last_take_for` walks through `Switch.input`.
    #[test]
    fn last_take_absorb_through_switch() {
        let table = switch_table_with(
            [(SwitchKey::Int(1), Value::Text("one".into()))],
            Some(Value::Text("unknown".into())),
        );
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 0 },
                TransformOp::Switch {
                    input: Box::new(TransformOp::Take { source_index: 0 }),
                    table,
                    truncate: false,
                },
            ],
            Vec::new(),
        );
        assert_eq!(t.last_take_for, vec![Some(1)]);

        let Row {
            mut values,
            mut body,
            ..
        } = raw_row(vec![Value::Int32(1)]);
        let v0 = t.eval_op(&t.cols[0], 0, &mut values, &mut body).unwrap();
        assert_eq!(v0, Value::Int32(1));
        assert_eq!(values[0], Value::Int32(1));

        let v1 = t.eval_op(&t.cols[1], 1, &mut values, &mut body).unwrap();
        assert_eq!(v1, Value::Text("one".into()));
        // The Switch.input::Take absorbed the source value because it
        // is the last reference to source_index 0.
        assert_eq!(values[0], Value::Null);
    }

    /// Integer-subtype canonicalisation must hold at runtime too: an
    /// operator-written `1` (compiled as `SwitchKey::Int(1)`) must match
    /// a `Value::Int64(1)` source.
    #[test]
    fn switch_cross_int_subtype_hit() {
        let table = switch_table_with([(SwitchKey::Int(1), Value::Text("one".into()))], None);
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            Vec::new(),
        );
        let raw = batch_of(vec![raw_row(vec![Value::Int64(1)])]);
        let batch = t.apply(raw).unwrap();
        assert_eq!(batch.rows[0].values, vec![Value::Text("one".into())]);
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
        let err = compile_to_transform(
            &expanded,
            DataType::Int32,
            &[],
            &body_conversions,
            &read_columns,
            false,
        )
        .unwrap_err();
        // Pin the specific failure mode: non-object body surfaces as a
        // `transform:compile` invariant. A regression that started
        // emitting a length-mismatch (or any other variant) would have
        // slipped past a bare `is_err()` check.
        assert!(
            matches!(
                err,
                ValidationError::AccessFailed {
                    component: "transform:compile",
                    ..
                }
            ),
            "expected AccessFailed/transform:compile, got {err:?}"
        );
    }

    /// `TransformOp::output_type` for `Convert` always returns
    /// `plan.sink` — gating is delegated to `CompatibilityValidator`.
    #[test]
    fn output_type_convert_returns_plan_sink() {
        let plan = ColumnConversionPlan {
            source: Some(DataType::Int64),
            sink: DataType::Int32,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let op = TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan,
            truncate: false,
        };
        let out = op.output_type(&DataType::Int64).unwrap();
        assert_eq!(out, DataType::Int32);
    }

    /// `TransformOp::output_type` rejection branch — `Switch` fed a
    /// source DataType the dispatcher cannot canonicalise (Json).
    #[test]
    fn output_type_switch_rejects_unswitchable_source() {
        use crate::transform::{SwitchKey, SwitchTable};
        let table = SwitchTable {
            cases: ahash::AHashMap::from_iter([(
                SwitchKey::Text("x".into()),
                Value::Text("y".into()),
            )]),
            default: None,
            output_type: DataType::Text { size: None },
        };
        let op = TransformOp::Switch {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            table,
            truncate: false,
        };
        let err = op.output_type(&DataType::Json).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchUnsupportedSource { .. }
        ));
    }

    /// `truncate_flag()`: Take/Body always false, Convert/Switch carry
    /// the op's declared flag.
    #[test]
    fn truncate_flag_per_op_variant() {
        assert!(!TransformOp::Take { source_index: 0 }.truncate_flag());
        assert!(!TransformOp::Body.truncate_flag());
        let plan = ColumnConversionPlan::identity(DataType::Int32);
        assert!(
            !TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan: plan.clone(),
                truncate: false
            }
            .truncate_flag()
        );
        assert!(
            TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan,
                truncate: true
            }
            .truncate_flag()
        );
    }

    // ---- Dynamic Convert (plan.source = None) — schemaless source ----

    /// A `Convert { plan.source = None }` op targeting `Int64` accepts
    /// heterogeneous `Value` variants across rows and produces `Int64`
    /// for each one. This is the runtime invariant that frees schemaless
    /// sources from the sampled-schema contract: 99 docs with `Int32`
    /// plus one `Int64` no longer blow up.
    #[test]
    fn dynamic_convert_dispatches_per_value_variant() {
        let plan = ColumnConversionPlan {
            source: None,
            sink: DataType::Int64,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let op = TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan,
            truncate: false,
        };
        let t = Transform::new(vec![op], vec!["n".into()]);

        // Batch 1: Int32 cells.
        let raw1 = batch_of(vec![raw_row(vec![Value::Int32(42)])]);
        let out1 = t.apply(raw1).unwrap();
        assert_eq!(out1.rows[0].values, vec![Value::Int64(42)]);

        // Batch 2: Int64 cells — same Transform program, no recompile.
        let raw2 = batch_of(vec![raw_row(vec![Value::Int64(9_000_000_000)])]);
        let out2 = t.apply(raw2).unwrap();
        assert_eq!(out2.rows[0].values, vec![Value::Int64(9_000_000_000)]);

        // Batch 3: Int8 cell — pure widening to Int64.
        let raw3 = batch_of(vec![raw_row(vec![Value::Int8(7)])]);
        let out3 = t.apply(raw3).unwrap();
        assert_eq!(out3.rows[0].values, vec![Value::Int64(7)]);
    }

    /// `compile_to_transform(source_schemaless = true)` emits a
    /// dynamic-source `Convert { plan.source = None }` for direct
    /// mappings — even when the sampled `plan.source` would otherwise
    /// match the sink (identity). The static `Take` collapse is unsafe
    /// under schemaless sources because the next document may carry a
    /// different variant.
    #[test]
    fn schemaless_compile_emits_dynamic_convert_even_for_identity_plan() {
        use crate::mapping::{DirectMapping, ExpandedMapping};
        use crate::model::ColumnConversionPlan;
        use crate::transform::compile_to_transform;

        let expanded = ExpandedMapping {
            direct: vec![DirectMapping {
                from: "n".into(),
                to: "n".into(),
                truncate: false,
                default_literal: None,
                switch: None,
            }],
            body: None,
        };
        // Identity-looking plan but `source: None` — under schemaless
        // the compiler must still emit dynamic-source Convert.
        let plan = ColumnConversionPlan {
            source: None,
            sink: DataType::Int64,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let read_columns = vec!["n".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            true,
        )
        .unwrap();
        match &t.cols[0] {
            TransformOp::Convert { plan, .. } => {
                assert!(plan.source.is_none(), "expected dynamic-source plan");
                assert_eq!(plan.sink, DataType::Int64);
            }
            other => panic!("expected dynamic Convert, got {other:?}"),
        }
    }

    /// Companion negative: `source_schemaless = false` with the same
    /// identity plan collapses to a bare `Take` (existing fast path).
    /// Confirms the typed-source path is unchanged.
    #[test]
    fn typed_compile_keeps_identity_take_path() {
        use crate::mapping::{DirectMapping, ExpandedMapping};
        use crate::model::ColumnConversionPlan;
        use crate::transform::compile_to_transform;

        let expanded = ExpandedMapping {
            direct: vec![DirectMapping {
                from: "n".into(),
                to: "n".into(),
                truncate: false,
                default_literal: None,
                switch: None,
            }],
            body: None,
        };
        let plan = ColumnConversionPlan::identity(DataType::Int64);
        let read_columns = vec!["n".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            false,
        )
        .unwrap();
        assert!(matches!(&t.cols[0], TransformOp::Take { source_index: 0 }));
    }

    /// End-to-end through `compile_to_transform`: a flow compiled with
    /// `source_schemaless=true` and a sink column of type `BigInt`
    /// accepts batches where the actual cell variants differ between
    /// batches (Int32 in one, Int64 in the next). Both must succeed —
    /// the sampled "source type" never enters the runtime.
    #[test]
    fn schemaless_flow_accepts_cross_batch_value_drift() {
        use crate::mapping::{DirectMapping, ExpandedMapping};
        use crate::model::ColumnConversionPlan;
        use crate::transform::compile_to_transform;
        use num_bigint::BigInt;

        let expanded = ExpandedMapping {
            direct: vec![DirectMapping {
                from: "n".into(),
                to: "n".into(),
                truncate: false,
                default_literal: None,
                switch: None,
            }],
            body: None,
        };
        // Sample said Int32 — but that's a hypothesis. The compiler
        // for a schemaless source must NOT honour it as the runtime
        // source type. Sink is BigInt (arbitrary-precision). We pass
        // `source: None` to model the schemaless plan that
        // `build_conversions` will emit for `src_schemaless == true`.
        let plan = ColumnConversionPlan {
            source: None,
            sink: DataType::BigInt { width: None },
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let read_columns = vec!["n".to_string()];
        let t = compile_to_transform(
            &expanded,
            DataType::Json,
            std::slice::from_ref(&plan),
            &[],
            &read_columns,
            true,
        )
        .unwrap();

        // Batch 1: matches the sample's hypothesis (Int32).
        let b1 = batch_of(vec![raw_row(vec![Value::Int32(42)])]);
        let r1 = t.apply(b1).unwrap();
        assert_eq!(r1.rows[0].values, vec![Value::BigInt(BigInt::from(42))]);

        // Batch 2: drift — actual Int64 cell. The static plan path
        // would have failed with `ValueShapeMismatch` here.
        let b2 = batch_of(vec![raw_row(vec![Value::Int64(9_000_000_000)])]);
        let r2 = t.apply(b2).unwrap();
        assert_eq!(
            r2.rows[0].values,
            vec![Value::BigInt(BigInt::from(9_000_000_000_i64))]
        );

        // Batch 3: further drift — actual Int8 cell. Still fine.
        let b3 = batch_of(vec![raw_row(vec![Value::Int8(7)])]);
        let r3 = t.apply(b3).unwrap();
        assert_eq!(r3.rows[0].values, vec![Value::BigInt(BigInt::from(7))]);
    }

    /// Null short-circuit on dynamic-source `Convert`: the dispatcher
    /// treats `Value::Null` before inspecting `src`, and the default is
    /// substituted when present.
    #[test]
    fn dynamic_convert_null_default_substitution() {
        let mut ctx = ConversionContext::passthrough();
        ctx.default = Some(Value::Int64(99));
        let plan = ColumnConversionPlan {
            source: None,
            sink: DataType::Int64,
            ctx,
            switch: None,
        };
        let op = TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan,
            truncate: false,
        };
        let t = Transform::new(vec![op], vec!["n".into()]);
        let raw = batch_of(vec![raw_row(vec![Value::Null])]);
        let out = t.apply(raw).unwrap();
        assert_eq!(out.rows[0].values, vec![Value::Int64(99)]);
    }

    /// `Convert.output_type` returns `plan.sink` regardless of whether
    /// the plan is static (`source = Some`) or dynamic (`source = None`).
    /// At validation time we may have no source schema for schemaless
    /// sources.
    #[test]
    fn dynamic_convert_output_type_returns_sink_only() {
        let plan = ColumnConversionPlan {
            source: None,
            sink: DataType::Text { size: None },
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let op = TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan,
            truncate: false,
        };
        // Pass an unused `input` DataType: the op must not consult it.
        let out = op.output_type(&DataType::Int32).unwrap();
        assert_eq!(out, DataType::Text { size: None });
    }

    /// Real cross-batch source-type drift on `Convert { source: None }`:
    /// an `Int32` cell in one batch and an `Int64` cell in the next both
    /// flow through cleanly into the `BigInt` sink. This is the
    /// schemaless invariant the unified `Convert` arm protects.
    #[test]
    fn dynamic_convert_handles_cross_batch_value_type_drift() {
        use num_bigint::BigInt;

        let plan = ColumnConversionPlan {
            source: None,
            sink: DataType::BigInt { width: None },
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let op = TransformOp::Convert {
            input: Box::new(TransformOp::Take { source_index: 0 }),
            plan,
            truncate: false,
        };
        let t = Transform::new(vec![op], vec!["n".into()]);

        let b1 = batch_of(vec![raw_row(vec![Value::Int32(42)])]);
        let r1 = t.apply(b1).unwrap();
        assert_eq!(r1.rows[0].values, vec![Value::BigInt(BigInt::from(42))]);

        let b2 = batch_of(vec![raw_row(vec![Value::Int64(9_000_000_000)])]);
        let r2 = t.apply(b2).unwrap();
        assert_eq!(
            r2.rows[0].values,
            vec![Value::BigInt(BigInt::from(9_000_000_000_i64))]
        );
    }
}
