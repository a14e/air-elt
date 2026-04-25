use crate::error::{TypeError, ValidationError};
use crate::mapping::ColumnMapping;
use crate::model::Schema;
use crate::types::matrix;

/// Verify that every mapped column exists on both sides and that the source's
/// canonical type is compatible with the sink's canonical type.
///
/// Actual native-to-canonical conversion lives in each connector's `model/`
/// module. This check enforces only the canonical-level compatibility rules
/// (identity + safe widening + null assignability).
pub fn check_mapping(
    source_schema: &Schema,
    sink_schema: &Schema,
    mappings: &[ColumnMapping],
) -> Result<(), ValidationError> {
    for m in mappings {
        let src_field =
            source_schema
                .find(&m.from)
                .ok_or_else(|| ValidationError::MissingField {
                    side: "source",
                    field: m.from.clone(),
                })?;
        let sink_field = sink_schema
            .find(&m.to)
            .ok_or_else(|| ValidationError::MissingField {
                side: "sink",
                field: m.to.clone(),
            })?;

        if !matrix::is_compatible(src_field.data_type, sink_field.data_type) {
            let type_err = if matrix::is_narrowing(src_field.data_type, sink_field.data_type) {
                TypeError::NarrowingNotAllowed {
                    from: src_field.data_type,
                    to: sink_field.data_type,
                }
            } else {
                TypeError::UnsupportedCast {
                    from: src_field.data_type,
                    to: sink_field.data_type,
                }
            };
            return Err(ValidationError::IncompatibleTypes {
                field: format!("{} -> {}", m.from, m.to),
                from: src_field.data_type,
                to: sink_field.data_type,
                source: type_err,
            });
        }

        // Nullability: if source allows null but sink doesn't, reject with a
        // dedicated error variant so the message doesn't claim "no cast from
        // Int32 to Int32" when the types are identical.
        if src_field.nullable && !sink_field.nullable {
            return Err(ValidationError::NullabilityMismatch {
                field: format!("{} -> {}", m.from, m.to),
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

    fn mapping(from: &str, to: &str) -> ColumnMapping {
        ColumnMapping {
            from: from.into(),
            to: to.into(),
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
    fn narrowing_mapping_rejected() {
        let src = Schema::new(vec![Field {
            name: "v".into(),
            data_type: DataType::Int64,
            nullable: false,
        }]);
        let dst = Schema::new(vec![Field {
            name: "v".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let err = check_mapping(&src, &dst, &[mapping("v", "v")]).unwrap_err();
        assert!(matches!(err, ValidationError::IncompatibleTypes { .. }));
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
