use crate::error::ValidationError;
use crate::mapping::DirectMapping;
use crate::model::Schema;

/// Structural mapping guard: every mapped column must exist on both
/// sides, and a nullable source feeding a NOT-NULL sink must carry a
/// `default` to bridge the gap.
///
/// Type-compatibility checking has moved into
/// [`crate::validation::compatibility::CompatibilityValidator`], which
/// compares each post-transform output `DataType` (resolved via
/// `Transform::resolve_types`) against the sink column. This split lets
/// cross-family flows (e.g. `Bool → Text` via `Switch`) succeed where
/// the old source-vs-sink matrix would have rejected them.
///
/// Operates on the post-expansion [`DirectMapping`] vector. Sink-driven
/// wildcard expansion already filters out nullable sink columns that
/// have no source counterpart — every entry reaching this check has a
/// real source column.
pub fn check_mapping(
    source_schema: &Schema,
    sink_schema: &Schema,
    mappings: &[DirectMapping],
) -> Result<(), ValidationError> {
    for m in mappings {
        let DirectMapping { from, to, .. } = m;
        let src_field = source_schema
            .find(from)
            .ok_or_else(|| ValidationError::MissingField {
                side: "source",
                field: from.clone(),
            })?;
        let sink_field = sink_schema
            .find(to)
            .ok_or_else(|| ValidationError::MissingField {
                side: "sink",
                field: to.clone(),
            })?;

        // Nullability: if source allows null but sink doesn't, a
        // `default` bridges the gap. Without one, reject with a
        // dedicated error. Long-form `default` lookup is performed
        // by the caller (validation::pipeline) since the parsed
        // default literal is built there alongside the
        // ColumnConversionPlan; we only check the structural shape here.
        let default_present = m.default_literal.is_some();
        if src_field.nullable && !sink_field.nullable && !default_present {
            return Err(ValidationError::NullabilityMismatch {
                field: format!("{from} -> {to}"),
                source_nullable: src_field.nullable,
                sink_nullable: sink_field.nullable,
            });
        }
    }
    Ok(())
}

/// Cursor fields must exist in the source schema.
pub fn check_cursor(
    flow: &str,
    source_schema: &Schema,
    cursor_fields: &[String],
) -> Result<(), ValidationError> {
    for field in cursor_fields {
        if source_schema.find(field).is_none() {
            return Err(ValidationError::MissingCursorField {
                flow: flow.to_string(),
                field: field.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::model::Field;
    use crate::types::DataType;

    fn mapping(from: &str, to: &str) -> DirectMapping {
        DirectMapping {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default_literal: None,
            switch: None,
        }
    }

    #[test]
    fn compatible_mapping_accepted() {
        let src = Schema::new(vec![Field {
            name: "id".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let dst = Schema::new(vec![Field {
            name: "id".into(),
            data_type: DataType::Int64,
            nullable: false,
        }]);
        check_mapping(&src, &dst, &[mapping("id", "id")]).unwrap();
    }

    #[test]
    fn missing_sink_field_rejected() {
        let src = Schema::new(vec![Field {
            name: "id".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let dst = Schema::new(vec![]);
        let err = check_mapping(&src, &dst, &[mapping("id", "id")]).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::MissingField { side: "sink", .. }
        ));
    }

    #[test]
    fn nullability_mismatch_rejected() {
        let src = Schema::new(vec![Field {
            name: "col".into(),
            data_type: DataType::Int32,
            nullable: true,
        }]);
        let dst = Schema::new(vec![Field {
            name: "col".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let err = check_mapping(&src, &dst, &[mapping("col", "col")]).unwrap_err();
        assert!(matches!(
            err,
            ValidationError::NullabilityMismatch {
                source_nullable: true,
                sink_nullable: false,
                ..
            }
        ));
    }

    #[test]
    fn cursor_must_exist_in_source() {
        let src = Schema::new(vec![Field {
            name: "id".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        check_cursor("f", &src, &["id".to_string()]).unwrap();
        let err = check_cursor("f", &src, &["missing".to_string()]).unwrap_err();
        assert!(matches!(err, ValidationError::MissingCursorField { .. }));
    }
}
