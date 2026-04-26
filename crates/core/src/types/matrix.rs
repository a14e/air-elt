use crate::types::data_type::DataType;

/// Compatibility predicate for validation.
///
/// Each connector owns its `native_type ↔ DataType` mapping; this matrix only
/// answers: "if the source produces `source_t`, can a sink whose column
/// canonicalises to `sink_t` accept it?". Rejected pairings fail at
/// `validate`, before any runtime work.
///
/// Rules:
/// * exact match always allowed (modulo size widening for `Text`/`Bytes`);
/// * integer and float widening allowed;
/// * `Text(a) → Text(b)` requires `b` unbounded *or* (`a` bounded ∧ `a ≤ b`)
///   — same for `Bytes`. No narrowing, no unbounded-into-bounded;
/// * UUID round-trips through text/bytes when the sink is wide enough
///   (`Text ≥ 36`, `Bytes ≥ 16`); reverse direction is allowed under the same
///   width rule (final parse validation deferred to runtime in `convert`);
/// * `Int* ↔ Bool` allowed (runtime conversion: `0 ↔ false`, non-zero → true);
/// * everything else requires an exact match.
pub fn is_compatible(source_t: DataType, sink_t: DataType) -> bool {
    use DataType::*;

    if source_t == sink_t {
        return true;
    }

    match (source_t, sink_t) {
        // Integer widening
        (Int16, Int32) | (Int16, Int64) | (Int32, Int64) => true,
        // Float widening
        (Float32, Float64) => true,
        // Int-to-float widening (lossless mantissa fits)
        (Int16, Float32) | (Int16, Float64) | (Int32, Float64) => true,

        // Text size widening: Text(a) → Text(b)
        (Text { size: a }, Text { size: b }) => fits_size(a, b),
        // Bytes size widening
        (Bytes { size: a }, Bytes { size: b }) => fits_size(a, b),

        // UUID ↔ text (CHAR(36)) — needs ≥ 36 bytes either way
        (Uuid, Text { size: b }) => b.is_none_or(|n| n >= 36),
        (Text { size: a }, Uuid) => a.is_none_or(|n| n >= 36),

        // UUID ↔ bytes (BINARY(16)) — needs ≥ 16 bytes either way
        (Uuid, Bytes { size: b }) => b.is_none_or(|n| n >= 16),
        (Bytes { size: a }, Uuid) => a.is_none_or(|n| n >= 16),

        // Int ↔ Bool runtime coercion
        (Int16 | Int32 | Int64, Bool) => true,
        (Bool, Int16 | Int32 | Int64) => true,

        _ => false,
    }
}

/// `Text(a) → Text(b)` (or `Bytes`) compatibility on size only.
/// Bounded → bounded ok if `a ≤ b`. Bounded → unbounded ok. Unbounded →
/// bounded **rejected** (potential overflow). Unbounded → unbounded ok.
fn fits_size(source: Option<u32>, sink: Option<u32>) -> bool {
    match (source, sink) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(a), Some(b)) => a <= b,
    }
}

pub fn is_narrowing(source_t: DataType, sink_t: DataType) -> bool {
    use DataType::*;
    if let (Text { size: Some(a) }, Text { size: Some(b) }) = (source_t, sink_t) {
        return a > b;
    }
    if let (Bytes { size: Some(a) }, Bytes { size: Some(b) }) = (source_t, sink_t) {
        return a > b;
    }
    if let (Text { size: None }, Text { size: Some(_) }) = (source_t, sink_t) {
        return true;
    }
    if let (Bytes { size: None }, Bytes { size: Some(_) }) = (source_t, sink_t) {
        return true;
    }
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

    const TEXT: DataType = DataType::text();
    const BYTES: DataType = DataType::bytes();

    fn text(n: u32) -> DataType {
        DataType::Text { size: Some(n) }
    }

    fn bytes(n: u32) -> DataType {
        DataType::Bytes { size: Some(n) }
    }

    #[test]
    fn identical_types_compatible() {
        assert!(is_compatible(DataType::Int32, DataType::Int32));
        assert!(is_compatible(TEXT, TEXT));
        assert!(is_compatible(text(36), text(36)));
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
    fn int_to_bool_allowed_now() {
        assert!(is_compatible(DataType::Int16, DataType::Bool));
        assert!(is_compatible(DataType::Int32, DataType::Bool));
        assert!(is_compatible(DataType::Int64, DataType::Bool));
        assert!(is_compatible(DataType::Bool, DataType::Int32));
    }

    #[test]
    fn distinct_scalars_not_auto_compatible() {
        assert!(!is_compatible(TEXT, DataType::Json));
        assert!(!is_compatible(DataType::Date, DataType::Timestamp));
    }

    #[test]
    fn int_to_float_lossy_rejected() {
        assert!(!is_compatible(DataType::Int32, DataType::Float32));
        assert!(!is_compatible(DataType::Float64, DataType::Int64));
    }

    #[test]
    fn int_to_float_widening_allowed() {
        assert!(is_compatible(DataType::Int16, DataType::Float32));
        assert!(is_compatible(DataType::Int16, DataType::Float64));
        assert!(is_compatible(DataType::Int32, DataType::Float64));
    }

    #[test]
    fn text_widening_allowed() {
        assert!(is_compatible(text(10), text(20)));
        assert!(is_compatible(text(10), TEXT));
        assert!(is_compatible(text(36), text(36)));
    }

    #[test]
    fn text_narrowing_rejected() {
        assert!(!is_compatible(text(20), text(10)));
        assert!(!is_compatible(TEXT, text(255)));
        assert!(is_narrowing(text(20), text(10)));
        assert!(is_narrowing(TEXT, text(255)));
    }

    #[test]
    fn bytes_widening_allowed() {
        assert!(is_compatible(bytes(10), bytes(20)));
        assert!(is_compatible(bytes(10), BYTES));
        assert!(!is_compatible(bytes(20), bytes(10)));
        assert!(!is_compatible(BYTES, bytes(16)));
    }

    #[test]
    fn uuid_to_text_requires_36() {
        assert!(is_compatible(DataType::Uuid, text(36)));
        assert!(is_compatible(DataType::Uuid, text(64)));
        assert!(is_compatible(DataType::Uuid, TEXT));
        assert!(!is_compatible(DataType::Uuid, text(35)));
    }

    #[test]
    fn uuid_to_bytes_requires_16() {
        assert!(is_compatible(DataType::Uuid, bytes(16)));
        assert!(is_compatible(DataType::Uuid, bytes(32)));
        assert!(is_compatible(DataType::Uuid, BYTES));
        assert!(!is_compatible(DataType::Uuid, bytes(15)));
    }

    #[test]
    fn text_to_uuid_requires_36() {
        assert!(is_compatible(text(36), DataType::Uuid));
        assert!(is_compatible(TEXT, DataType::Uuid));
        assert!(!is_compatible(text(35), DataType::Uuid));
    }

    #[test]
    fn bytes_to_uuid_requires_16() {
        assert!(is_compatible(bytes(16), DataType::Uuid));
        assert!(is_compatible(BYTES, DataType::Uuid));
        assert!(!is_compatible(bytes(8), DataType::Uuid));
    }

    #[test]
    fn bytes_text_incompatible_both_directions() {
        assert!(!is_compatible(BYTES, TEXT));
        assert!(!is_compatible(TEXT, BYTES));
    }

    #[test]
    fn json_text_incompatible_both_directions() {
        assert!(!is_compatible(DataType::Json, TEXT));
        assert!(!is_compatible(TEXT, DataType::Json));
    }

    #[test]
    fn date_timestamp_incompatible_both_directions() {
        assert!(!is_compatible(DataType::Date, DataType::Timestamp));
        assert!(!is_compatible(DataType::Timestamp, DataType::Date));
    }
}
