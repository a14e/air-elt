//! Sink-aware compatibility validation for a compiled `Transform`.
//!
//! `CompatibilityValidator` borrows a compiled `Transform` plus the
//! source-side resolution inputs and validates that each post-transform
//! output `DataType` is acceptable to the corresponding sink column.
//!
//! Truncate is consulted **per-column** through `TransformOp::truncate_flag`:
//! only columns whose op actively declared `truncate = true` are
//! checked against the truncate-augmented matrix. The truncate matrix
//! is a **true super-relation** of the lossless one (every lossless pair
//! is admitted there as well), so a `truncate=true` column whose
//! `plan.sink` already matches the schema slot losslessly still
//! validates — `truncate=true` means "I accept lossy narrowing if any
//! happens", not "I demand narrowing". The narrow per-column gate is
//! still important: dispatching to the truncate matrix unconditionally
//! would silently admit lossy pairs on any future op that bypasses
//! `TransformOp::output_type`.
//!
//! Schemaless sinks short-circuit: with no declared sink schema there
//! is nothing to validate against.

use crate::error::{RuntimeError, TypeError, ValidationError};
use crate::model::Schema;
use crate::transform::Transform;
use crate::types::{DataType, matrix};

pub struct CompatibilityValidator<'a> {
    flow_name: &'a str,
    transform: &'a Transform,
    source_schema: &'a Schema,
    source_body_type: &'a DataType,
}

impl<'a> CompatibilityValidator<'a> {
    pub fn new(
        flow_name: &'a str,
        transform: &'a Transform,
        source_schema: &'a Schema,
        source_body_type: &'a DataType,
    ) -> Self {
        Self {
            flow_name,
            transform,
            source_schema,
            source_body_type,
        }
    }

    /// Single-pass: resolve + check against sink. No-op when `schemaless`.
    /// `sink_columns` must align 1:1 with `transform.cols` (one entry per
    /// post-transform column). Missing sink fields are reported.
    pub fn validate(
        &self,
        sink_schema: &Schema,
        sink_columns: &[String],
        schemaless: bool,
    ) -> Result<(), ValidationError> {
        let resolved = self
            .transform
            .resolve_types(self.source_schema, self.source_body_type)?;
        if schemaless {
            return Ok(());
        }
        if resolved.len() != sink_columns.len() {
            return Err(ValidationError::AccessFailed {
                component: "validation:compatibility",
                name: self.flow_name.to_string(),
                source: Box::new(RuntimeError::DerivedPlanInvariant {
                    detail: format!(
                        "resolved types ({}) != sink columns ({})",
                        resolved.len(),
                        sink_columns.len()
                    ),
                }),
            });
        }
        for ((col_idx, out_dt), col_name) in resolved.iter().enumerate().zip(sink_columns.iter()) {
            let sink_field =
                sink_schema
                    .find(col_name)
                    .ok_or_else(|| ValidationError::MissingField {
                        side: "sink",
                        field: col_name.clone(),
                    })?;
            // Truncate is per-column and declarative — consult the op's
            // `truncate_flag()` rather than OR'ing both matrices. Take
            // / Body leaves never opt in (they carry no `truncate`
            // field), so a narrowing source-vs-sink that slipped
            // through `output_type` is rejected here.
            let truncate = self.transform.cols[col_idx].truncate_flag();
            let compatible = if truncate {
                matrix::is_compatible_with_truncate(out_dt.clone(), sink_field.data_type.clone())
            } else {
                matrix::is_compatible(out_dt.clone(), sink_field.data_type.clone())
            };
            if !compatible {
                let err = if matrix::is_narrowing(out_dt.clone(), sink_field.data_type.clone()) {
                    TypeError::NarrowingNotAllowed {
                        from: out_dt.clone(),
                        to: sink_field.data_type.clone(),
                    }
                } else {
                    TypeError::UnsupportedCast {
                        from: out_dt.clone(),
                        to: sink_field.data_type.clone(),
                    }
                };
                return Err(ValidationError::IncompatibleTypes {
                    field: col_name.clone(),
                    from: out_dt.clone(),
                    to: sink_field.data_type.clone(),
                    source: err,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::{ColumnConversionPlan, Field};
    use air_elt_types::Key;

    use crate::transform::{SwitchTable, Transform, TransformOp};
    use crate::types::{ConversionContext, Value};

    fn schema_of(fields: &[(&str, DataType)]) -> Schema {
        Schema::new(
            fields
                .iter()
                .map(|(n, dt)| Field {
                    name: (*n).into(),
                    data_type: dt.clone(),
                    nullable: false,
                })
                .collect(),
        )
    }

    /// Typed sink, identity column: `Int32 → Int32` resolves cleanly.
    #[test]
    fn typed_sink_identity_take_accepts_matching_types() {
        let src = schema_of(&[("a", DataType::Int32)]);
        let sink = schema_of(&[("a", DataType::Int32)]);
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["a".into()],
        );
        let body = DataType::Json;
        let v = CompatibilityValidator::new("f", &t, &src, &body);
        v.validate(&sink, &["a".into()], false).unwrap();
    }

    /// Typed sink, cross-family switch (`Bool → Text`): rejected by the
    /// source-vs-sink matrix but accepted by `output_type vs sink` —
    /// this is exactly the gap C2 closes.
    #[test]
    fn typed_sink_switch_bool_to_text_accepted() {
        let src = schema_of(&[("flag", DataType::Bool)]);
        let sink = schema_of(&[("flag_label", DataType::Text { size: None })]);
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
            default: Some(Value::Text("?".into())),
            output_type: DataType::Text { size: None },
        };
        let t = Transform::new(
            vec![TransformOp::Switch {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                table,
                truncate: false,
            }],
            vec!["flag".into()],
        );
        let body = DataType::Json;
        let v = CompatibilityValidator::new("f", &t, &src, &body);
        v.validate(&sink, &["flag_label".into()], false).unwrap();
    }

    /// Typed sink, widening Convert: `Int16 → Int64`.
    #[test]
    fn typed_sink_convert_widening_accepted() {
        let src = schema_of(&[("a", DataType::Int16)]);
        let sink = schema_of(&[("a", DataType::Int64)]);
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
            vec!["a".into()],
        );
        let body = DataType::Json;
        let v = CompatibilityValidator::new("f", &t, &src, &body);
        v.validate(&sink, &["a".into()], false).unwrap();
    }

    /// Missing sink column for a resolved output → `MissingField`.
    #[test]
    fn typed_sink_missing_column_reports_missing_field() {
        let src = schema_of(&[("a", DataType::Int32)]);
        let sink = schema_of(&[("b", DataType::Int32)]);
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["a".into()],
        );
        let body = DataType::Json;
        let err = CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &["a".into()], false)
            .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::MissingField { side: "sink", .. }
        ));
    }

    /// Schemaless sink: validation is a no-op regardless of types.
    #[test]
    fn schemaless_sink_validate_returns_ok() {
        let src = schema_of(&[("a", DataType::Int32)]);
        let sink = Schema::schemaless();
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["a".into()],
        );
        let body = DataType::Json;
        CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &[], true)
            .unwrap();
    }

    /// `resolved.len() != sink_columns.len()` surfaces as a
    /// `DerivedPlanInvariant`. The flow name from the constructor must
    /// propagate into the error.
    #[test]
    fn length_mismatch_reports_derived_plan_invariant_with_flow_name() {
        let src = schema_of(&[("a", DataType::Int32)]);
        let sink = schema_of(&[("a", DataType::Int32)]);
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["a".into()],
        );
        let body = DataType::Json;
        let err = CompatibilityValidator::new("my_flow", &t, &src, &body)
            .validate(&sink, &["a".into(), "b".into()], false)
            .unwrap_err();
        match err {
            ValidationError::AccessFailed {
                component: "validation:compatibility",
                name,
                ..
            } => assert_eq!(name, "my_flow"),
            other => panic!("expected AccessFailed/validation:compatibility, got {other:?}"),
        }
    }

    /// Unsupported cast: `Json → Int32` is not in any matrix.
    #[test]
    fn unsupported_cast_rejected_via_resolved_output() {
        let src = schema_of(&[("v", DataType::Json)]);
        let sink = schema_of(&[("v", DataType::Int32)]);
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["v".into()],
        );
        let body = DataType::Json;
        let err = CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &["v".into()], false)
            .unwrap_err();
        assert!(matches!(err, ValidationError::IncompatibleTypes { .. }));
    }

    /// Narrowing rejection: `Int64 → Int32` without `truncate=true` is
    /// rejected end-to-end. Restores coverage of the old
    /// `narrowing_mapping_rejected` test that lived on the removed
    /// `check_mapping` matrix branch. Constructed as a `Take` (rather
    /// than `Convert`) so the validator's gate is the only thing
    /// rejecting — verifying the OR-with-truncate hole is closed.
    #[test]
    fn take_narrowing_rejected_without_truncate() {
        let src = schema_of(&[("v", DataType::Int64)]);
        let sink = schema_of(&[("v", DataType::Int32)]);
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["v".into()],
        );
        let body = DataType::Json;
        let err = CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &["v".into()], false)
            .unwrap_err();
        match err {
            ValidationError::IncompatibleTypes { source, .. } => {
                assert!(matches!(source, TypeError::NarrowingNotAllowed { .. }));
            }
            other => panic!("expected IncompatibleTypes/NarrowingNotAllowed, got {other:?}"),
        }
    }

    /// Regression: `resolve_op`'s `Take` arm used to index
    /// `source_schema.fields()` positionally by `source_index`. But the
    /// schema's natural field order (Mongo `AHashMap`, SQL
    /// `ordinal_position`) is independent of the projection order in
    /// `read_columns`. Here `source_schema.fields()` is in declaration
    /// order `[a, b, c]` while `read_columns` is the permuted `[c, a, b]`
    /// — `Take{0}` must resolve to `c`'s `Float64`, not `a`'s `Int32`.
    /// The buggy positional resolver would have returned `Int32` and the
    /// sink check `Int32 vs Float64` would mis-pass; the corrected
    /// name-based resolver returns `Float64` which matches the sink.
    #[test]
    fn resolve_op_take_uses_read_columns_name_not_schema_position() {
        let src = schema_of(&[
            ("a", DataType::Int32),
            ("b", DataType::Text { size: Some(64) }),
            ("c", DataType::Float64),
        ]);
        let sink = schema_of(&[
            ("c_sink", DataType::Float64),
            ("a_sink", DataType::Int32),
            ("b_sink", DataType::Text { size: Some(64) }),
        ]);
        // Projection = permutation of the schema's natural order.
        let read_columns = vec!["c".to_string(), "a".to_string(), "b".to_string()];
        let t = Transform::new(
            vec![
                TransformOp::Take { source_index: 0 },
                TransformOp::Take { source_index: 1 },
                TransformOp::Take { source_index: 2 },
            ],
            read_columns,
        );
        let body = DataType::Json;
        CompatibilityValidator::new("f", &t, &src, &body)
            .validate(
                &sink,
                &["c_sink".into(), "a_sink".into(), "b_sink".into()],
                false,
            )
            .unwrap();
    }

    /// Companion negative: if the resolver were still positional, a
    /// permuted `read_columns` would silently accept the wrong sink type
    /// pair. Here `read_columns = [c, a, b]` so `Take{0}` resolves to
    /// `Float64`. The sink column for slot 0 is `Int32` — the validator
    /// must reject. (The buggy positional resolver would have read
    /// `fields()[0] = a: Int32`, then `Int32 vs Int32` would
    /// erroneously pass.)
    #[test]
    fn resolve_op_take_permuted_read_columns_rejects_wrong_sink_type() {
        let src = schema_of(&[
            ("a", DataType::Int32),
            ("b", DataType::Text { size: Some(64) }),
            ("c", DataType::Float64),
        ]);
        let sink = schema_of(&[("x", DataType::Int32)]);
        let t = Transform::new(
            vec![TransformOp::Take { source_index: 0 }],
            vec!["c".into(), "a".into(), "b".into()],
        );
        let body = DataType::Json;
        let err = CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &["x".into()], false)
            .unwrap_err();
        assert!(matches!(err, ValidationError::IncompatibleTypes { .. }));
    }

    /// Regression: Mongo `Timestamp → Date with truncate=true` mapping.
    /// `Convert{Take{0}}` lowers the source `Timestamp` into a sink
    /// `Date` column. After resolution, `out_dt = plan.sink = Date`
    /// and `sink_field.data_type = Date`. With `truncate=true` the
    /// validator routes to `is_compatible_with_truncate(Date, Date)`,
    /// which must admit the pair — `truncate=true` here means "I
    /// accept the lossy narrowing happening inside the Convert", not
    /// "I demand narrowing at the validator gate too". Pre-fix, the
    /// truncate matrix was disjoint from the lossless one and rejected
    /// `(Date, Date)`.
    #[test]
    fn convert_timestamp_to_date_with_truncate_accepted() {
        let src = schema_of(&[("t", DataType::Timestamp)]);
        let sink = schema_of(&[("t", DataType::Date)]);
        let plan = ColumnConversionPlan {
            source: Some(DataType::Timestamp),
            sink: DataType::Date,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let t = Transform::new(
            vec![TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan,
                truncate: true,
            }],
            vec!["t".into()],
        );
        let body = DataType::Json;
        CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &["t".into()], false)
            .unwrap();
    }

    /// `truncate = true` on a Convert lets the same narrowing through —
    /// `output_type` returns `plan.sink` (Int32) which matches the sink
    /// schema literally.
    #[test]
    fn convert_narrowing_accepted_with_truncate() {
        let src = schema_of(&[("v", DataType::Int64)]);
        let sink = schema_of(&[("v", DataType::Int32)]);
        let plan = ColumnConversionPlan {
            source: Some(DataType::Int64),
            sink: DataType::Int32,
            ctx: ConversionContext::passthrough(),
            switch: None,
        };
        let t = Transform::new(
            vec![TransformOp::Convert {
                input: Box::new(TransformOp::Take { source_index: 0 }),
                plan,
                truncate: true,
            }],
            vec!["v".into()],
        );
        let body = DataType::Json;
        CompatibilityValidator::new("f", &t, &src, &body)
            .validate(&sink, &["v".into()], false)
            .unwrap();
    }
}
