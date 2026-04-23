use crate::types::data_type::DataType;

/// Compatibility predicate for validation.
///
/// There is **no** core-to-core value conversion. Each connector owns its
/// `native_type ↔ DataType` mapping, and values flow through the runner in
/// whatever canonical form the source emitted. This matrix only answers:
/// "if the source produces `source_t`, can a sink whose column canonicalises
/// to `sink_t` accept it?" Rejected pairings fail at `validate`, before any
/// runtime work.
///
/// Rules: exact match always allowed; NULL is assignable anywhere; integer and
/// float widening allowed; everything else requires the pair to be identical
/// (narrowing and bool↔int coercions are refused so users stay honest about
/// their schemas).
pub fn is_compatible(source_t: DataType, sink_t: DataType) -> bool {
    use DataType::*;

    if source_t == sink_t {
        return true;
    }
    matches!(
        (source_t, sink_t),
        (Int16, Int32)
            | (Int16, Int64)
            | (Int32, Int64)
            | (Float32, Float64)
            | (Int16, Float32)
            | (Int16, Float64)
            | (Int32, Float64)
    )
}

pub fn is_narrowing(source_t: DataType, sink_t: DataType) -> bool {
    use DataType::*;
    matches!(
        (source_t, sink_t),
        (Int32, Int16)
            | (Int64, Int16)
            | (Int64, Int32)
            | (Float64, Float32)
            | (Float32, Int16)
            | (Float32, Int32)
            | (Float32, Int64)
            | (Float64, Int16)
            | (Float64, Int32)
            | (Float64, Int64)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn identical_types_compatible() {
        assert!(is_compatible(DataType::Int32, DataType::Int32));
        assert!(is_compatible(DataType::Text, DataType::Text));
        assert!(is_compatible(DataType::Uuid, DataType::Uuid));
    }

    #[test]
    fn integer_widening_allowed() {
        assert!(is_compatible(DataType::Int16, DataType::Int32));
        assert!(is_compatible(DataType::Int16, DataType::Int64));
        assert!(is_compatible(DataType::Int32, DataType::Int64));
    }

    #[test]
    fn integer_narrowing_rejected() {
        assert!(!is_compatible(DataType::Int64, DataType::Int32));
        assert!(is_narrowing(DataType::Int64, DataType::Int32));
    }

    #[test]
    fn float_widening_allowed() {
        assert!(is_compatible(DataType::Float32, DataType::Float64));
        assert!(!is_compatible(DataType::Float64, DataType::Float32));
    }

    #[test]
    fn bool_to_int_rejected() {
        // no implicit 0/1 coercion — sources declaring bool must stay bool
        assert!(!is_compatible(DataType::Bool, DataType::Int32));
    }

    #[test]
    fn distinct_scalars_not_auto_compatible() {
        // text/uuid/date do not widen into each other
        assert!(!is_compatible(DataType::Text, DataType::Uuid));
        assert!(!is_compatible(DataType::Date, DataType::Timestamp));
    }
}
