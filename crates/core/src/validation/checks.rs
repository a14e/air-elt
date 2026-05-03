use crate::error::{TypeError, ValidationError};
use crate::mapping::{ColumnMapping, FieldPath};
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
    check_sink_uniqueness(mappings)?;
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

        // `truncate=true` opts the column into the wider compatibility
        // matrix (`is_compatible_with_truncate`); without it we only allow
        // the lossless set.
        let compatible = if m.truncate {
            matrix::is_compatible_with_truncate(
                src_field.data_type.clone(),
                sink_field.data_type.clone(),
            )
        } else {
            matrix::is_compatible(src_field.data_type.clone(), sink_field.data_type.clone())
        };
        if !compatible {
            let type_err = if matrix::is_narrowing(
                src_field.data_type.clone(),
                sink_field.data_type.clone(),
            ) {
                TypeError::NarrowingNotAllowed {
                    from: src_field.data_type.clone(),
                    to: sink_field.data_type.clone(),
                }
            } else {
                TypeError::UnsupportedCast {
                    from: src_field.data_type.clone(),
                    to: sink_field.data_type.clone(),
                }
            };
            return Err(ValidationError::IncompatibleTypes {
                field: format!("{} -> {}", m.from, m.to),
                from: src_field.data_type.clone(),
                to: sink_field.data_type.clone(),
                source: type_err,
            });
        }

        // Nullability: if source allows null but sink doesn't, a `default`
        // bridges the gap. Without one, reject with a dedicated error.
        if src_field.nullable && !sink_field.nullable && m.default_literal.is_none() {
            return Err(ValidationError::NullabilityMismatch {
                field: format!("{} -> {}", m.from, m.to),
                source_nullable: src_field.nullable,
                sink_nullable: sink_field.nullable,
            });
        }
    }
    Ok(())
}

/// Reject mapping configurations that would write into the same sink
/// field more than once, or into a sink field that is a parent /
/// ancestor of another mapped field (only meaningful for connectors
/// that build nested documents — e.g. MongoDB — where `to = "addr"`
/// and `to = "addr.city"` together would silently overwrite the
/// nested writer).
///
/// Mappings are tiny (handful to a few dozen entries), so the O(n²)
/// scan is fine.
fn check_sink_uniqueness(mappings: &[ColumnMapping]) -> Result<(), ValidationError> {
    let parsed: Vec<Option<FieldPath>> = mappings
        .iter()
        .map(|m| FieldPath::parse(&m.to).ok())
        .collect();

    for (i, mi) in mappings.iter().enumerate() {
        for (j, mj) in mappings.iter().enumerate().take(i) {
            if mi.to == mj.to {
                return Err(ValidationError::DuplicateSinkField {
                    field: mi.to.clone(),
                    first_index: j,
                    duplicate_index: i,
                    detail: String::new(),
                });
            }
            // Prefix conflict only meaningful when both `to` values
            // parse as valid paths. If one fails to parse we let the
            // per-column type check produce its own error.
            if let (Some(a), Some(b)) = (&parsed[j], &parsed[i]) {
                if a.is_nested() || b.is_nested() {
                    if a.is_prefix_or_equal(b) || b.is_prefix_or_equal(a) {
                        return Err(ValidationError::DuplicateSinkField {
                            field: format!("{} / {}", mj.to, mi.to),
                            first_index: j,
                            duplicate_index: i,
                            detail: " — one path is an ancestor of the other".to_string(),
                        });
                    }
                }
            }
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
            truncate: false,
            default_literal: None,
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
    fn duplicate_sink_field_rejected() {
        let src = Schema::new(vec![
            Field {
                name: "a".into(),
                data_type: DataType::Int32,
                nullable: false,
            },
            Field {
                name: "b".into(),
                data_type: DataType::Int32,
                nullable: false,
            },
        ]);
        let dst = Schema::new(vec![Field {
            name: "out".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let err =
            check_mapping(&src, &dst, &[mapping("a", "out"), mapping("b", "out")]).unwrap_err();
        assert!(matches!(err, ValidationError::DuplicateSinkField { .. }));
    }

    #[test]
    fn nested_path_prefix_rejected() {
        // The schema lookups would fail for nested paths (since SQL
        // schemas don't have nested fields), but the prefix check
        // runs first.
        let src = Schema::new(vec![
            Field {
                name: "a".into(),
                data_type: DataType::Int32,
                nullable: false,
            },
            Field {
                name: "b".into(),
                data_type: DataType::Int32,
                nullable: false,
            },
        ]);
        let dst = Schema::new(vec![]);
        let err = check_mapping(
            &src,
            &dst,
            &[mapping("a", "addr"), mapping("b", "addr.city")],
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::DuplicateSinkField { .. }));
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
