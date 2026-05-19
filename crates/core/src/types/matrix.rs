use crate::types::data_type::DataType;

/// Lossless compatibility predicate for validation.
///
/// Each connector owns its `native_type ↔ DataType` mapping; this matrix only
/// answers: "if the source produces `source_t`, can a sink whose column
/// canonicalises to `sink_t` accept it **without loss**?". Rejected
/// pairings fail at `validate`, before any runtime work.
///
/// [`is_compatible_with_truncate`] is a true super-relation of this
/// predicate: anything this function admits is also admitted there.
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

    // Identity short-circuit covers `Union(vs) → Union(vs)` (Mongo →
    // Mongo schemaless: `dst_schema` is rebuilt from `src_schema` so a
    // heterogeneous source field surfaces with the same Union on both
    // sides).
    if source_t == sink_t {
        return true;
    }

    // Union on the source side: every variant must independently be
    // compatible with the (concrete) sink.
    if let Union(vs) = &source_t {
        return vs.iter().all(|v| is_compatible(v.clone(), sink_t.clone()));
    }
    // Sinks never carry Union — schemaful sinks have concrete columns;
    // the schemaless Mongo sink also takes its types from the source
    // (which may itself be a Union, handled by the identity arm
    // above). A non-identity Union sink is a misconfiguration.
    if matches!(sink_t, Union(_)) {
        return false;
    }

    // Custom routing: if either side is a connector-specific opaque
    // type, delegate through the trait. Identity is the cheap pre-
    // check (handled by the `source_t == sink_t` arm above); reaching
    // here means at least one side is Custom and they are not equal.
    if let (Custom(a), Custom(b)) = (&source_t, &sink_t) {
        // Two distinct custom types — only `eq_dyn` knows whether
        // they actually represent the same descriptor (parametric
        // types may differ structurally).
        return a.eq_dyn(&**b);
    }
    if let Custom(a) = &source_t {
        return a.can_convert_to(&sink_t, false);
    }
    if let Custom(b) = &sink_t {
        return b.can_construct_from(&source_t, false);
    }

    match (source_t, sink_t) {
        // Integer widening: Int8 → Int16/32/64 lossless
        (Int8, Int16) | (Int8, Int32) | (Int8, Int64) => true,
        (Int16, Int32) | (Int16, Int64) | (Int32, Int64) => true,
        // Float widening
        (Float32, Float64) => true,
        // Int-to-float widening (lossless mantissa fits)
        (Int8, Float32) | (Int8, Float64) => true,
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
        (Int8 | Int16 | Int32 | Int64, Bool) => true,
        (Bool, Int8 | Int16 | Int32 | Int64) => true,

        // Unsigned widening: each step climbs one width. UInt8 also fits any
        // wider signed; UInt16 fits Int32+/Int64; UInt32 fits Int64. UInt64
        // does *not* fit any signed Int* (i64 max < u64 max), so it can only
        // widen into BigInt or wider unsigned.
        (UInt8, UInt16 | UInt32 | UInt64) => true,
        (UInt16, UInt32 | UInt64) => true,
        (UInt32, UInt64) => true,
        (UInt8, Int16 | Int32 | Int64) => true,
        (UInt16, Int32 | Int64) => true,
        (UInt32, Int64) => true,
        // Unsigned → BigInt: always lossless.
        (UInt8 | UInt16 | UInt32 | UInt64, BigInt { .. }) => true,
        // Unsigned → Decimal: digit widths 3 / 5 / 10 / 20.
        (UInt8, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 3),
        (UInt16, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 5),
        (UInt32, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 10),
        (UInt64, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 20),
        // Unsigned ↔ Bool: same algebra as signed Int ↔ Bool.
        (UInt8 | UInt16 | UInt32 | UInt64, Bool) => true,
        (Bool, UInt8 | UInt16 | UInt32 | UInt64) => true,

        // Fixed-width int → BigInt: always lossless.
        (Int8 | Int16 | Int32 | Int64, BigInt { .. }) => true,
        // BigInt → BigInt: only widen (target unbounded, or wider).
        (BigInt { width: a }, BigInt { width: b }) => fits_size(a, b),

        // Int → Decimal: ok when target precision-scale ≥ source digit width,
        // or target unbounded. Int8 max is 127 → 3 decimal digits.
        (Int8, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 3),
        (Int16, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 5),
        (Int32, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 10),
        (Int64, Decimal { precision, scale }) => decimal_fits_int_digits(precision, scale, 19),

        // BigInt → Decimal: target must be fully unbounded, or target's
        // integer-digits cover source width (or source unbounded only into
        // unbounded target).
        (BigInt { width: a }, Decimal { precision, scale }) => {
            decimal_fits_bigint(a, precision, scale)
        }

        // Decimal → Decimal: integer-digits and scale both widen (or target
        // fully unbounded).
        (
            Decimal {
                precision: pa,
                scale: sa,
            },
            Decimal {
                precision: pb,
                scale: sb,
            },
        ) => decimal_fits_decimal(pa, sa, pb, sb),

        // Decimal → Float64 / Float32 is NEVER lossless: even at
        // precision ≤ 15, fractional decimals like `0.1` have no exact
        // IEEE-754 binary representation. Operators who accept the
        // rounding must opt in via `truncate = true`; see
        // `is_compatible_with_truncate`. Reverse direction
        // (Float → Decimal) stays rejected on both matrices: lossy
        // float-to-fixed-point binding has no clean defaulting rule.

        // Xml/Json → unbounded text. Identity covers Xml→Xml and Json→Json.
        // `Xml → Text*` and `Json → Text*` (unbounded sink) are allowed
        // without truncate because no truncation is ever needed.
        // Bounded variants `* → Text(n)` are gated by
        // `is_compatible_with_truncate` and not allowed here.
        (Xml, Text { size: None }) => true,
        (Json, Text { size: None }) => true,
        // `Text → Xml` is permitted; the convert dispatcher validates
        // well-formedness at runtime via `quick-xml`.
        (Text { .. }, Xml) => true,

        // `Text → Bool` lexer accepts `y/t/1/true/yes` and `n/f/0/false/no`
        // (case-insensitive). Allowed without truncate — the value-set is
        // well-defined and small. Runtime may still raise `InvalidBool`.
        (Text { .. }, Bool) => true,

        // IP families. Ipv4 → Ipv6 is always lossless (IPv4-mapped
        // ::ffff:a.b.c.d). The reverse only succeeds for IPv4-mapped
        // addresses, so it requires truncate and lives in the truncate
        // matrix below.
        (Ipv4, Ipv6) => true,
        // IP ↔ Text — canonical decimal/colon-hex form ≤ 15 / 39 chars.
        (Ipv4, Text { size: b }) => b.is_none_or(|n| n >= 15),
        (Text { size: a }, Ipv4) => a.is_none_or(|n| n >= 7),
        (Ipv6, Text { size: b }) => b.is_none_or(|n| n >= 39),
        (Text { size: a }, Ipv6) => a.is_none_or(|n| n >= 2),
        // IP ↔ Bytes — network byte order (BE) octets per RFC 791/8200.
        (Ipv4, Bytes { size: b }) => b.is_none_or(|n| n >= 4),
        (Bytes { size: a }, Ipv4) => a.is_none_or(|n| n >= 4),
        (Ipv6, Bytes { size: b }) => b.is_none_or(|n| n >= 16),
        (Bytes { size: a }, Ipv6) => a.is_none_or(|n| n >= 16),

        // Narrowing from BigInt/Decimal back into fixed-width or integer
        // types is *not* supported — every reverse path is potentially
        // lossy. Users adding such pipelines must do an explicit transform.
        _ => false,
    }
}

/// Variant of [`is_compatible`] that is a **true super-relation** of the
/// lossless matrix: every pair `is_compatible` admits is also admitted
/// here, plus the additional narrowing pairs unlocked by the user's
/// explicit `truncate=true` opt-in. The super-relation invariant matters
/// at the validator: a transform that yields `plan.sink = T` against a
/// sink column of the same `T` must validate regardless of the
/// `truncate` flag — `truncate=true` is "I accept lossy narrowing if any
/// happens", never "I demand narrowing". Identity pairs `(T, T)` are the
/// canonical case (Mongo `Timestamp → Date` with `truncate=true` lands
/// the output as `Date`, and the validator then re-checks
/// `(Date, Date)`).
///
/// Forbidden truly-lossy combinations (e.g. `Date → Timestamp`) remain
/// `false` here too: no consent can rescue a cast that has no defined
/// runtime semantics.
pub fn is_compatible_with_truncate(source_t: DataType, sink_t: DataType) -> bool {
    use DataType::*;
    if let Union(vs) = &source_t {
        return vs
            .iter()
            .all(|v| is_compatible_with_truncate(v.clone(), sink_t.clone()));
    }
    if matches!(sink_t, Union(_)) {
        return false;
    }
    // Custom routing under truncate. Identity is delegated to
    // `is_compatible` below; for distinct `(Custom, Custom)` we
    // again compare via `eq_dyn`, with no special truncate semantics
    // (custom types decide their own narrowing rules through
    // `can_convert_to` / `can_construct_from`).
    if let (Custom(a), Custom(b)) = (&source_t, &sink_t) {
        return a.eq_dyn(&**b);
    }
    if let Custom(a) = &source_t {
        return a.can_convert_to(&sink_t, true);
    }
    if let Custom(b) = &sink_t {
        return b.can_construct_from(&source_t, true);
    }
    // Super-relation short-circuit: anything the lossless matrix admits
    // is admitted here unconditionally. Identity pairs `(T, T)` —
    // including `Json/Xml/Uuid/Date/Timestamp` — flow through this arm:
    // a `truncate=true` flag on an already-lossless mapping is a
    // harmless no-op, never a request to actually truncate. The runtime
    // dispatcher decides when to raise `TruncationForbidden`; the
    // validator must not pre-reject identity at type-check time.
    if is_compatible(source_t.clone(), sink_t.clone()) {
        return true;
    }

    match (source_t, sink_t) {
        (Date, Timestamp) => false,
        // UUID truncations don't have defined semantics — explicitly reject.
        (Uuid, Text { size: Some(n) }) if n < 36 => false,
        (Uuid, Bytes { size: Some(n) }) if n < 16 => false,
        // Text/Bytes narrowing: ok.
        (Text { .. }, Text { .. }) => true,
        (Bytes { .. }, Bytes { .. }) => true,
        // Signed → smaller signed, signed → unsigned (sat-to-zero), unsigned
        // → smaller unsigned, unsigned → smaller signed.
        (Int64, Int32 | Int16 | Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        (Int32, Int16 | Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        (Int16, Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        (Int8, UInt64 | UInt32 | UInt16 | UInt8) => true,
        (UInt64, UInt32 | UInt16 | UInt8 | Int64 | Int32 | Int16 | Int8) => true,
        (UInt32, UInt16 | UInt8 | Int32 | Int16 | Int8) => true,
        (UInt16, UInt8 | Int16 | Int8) => true,
        (UInt8, Int8) => true,
        // Float narrowing.
        (Float64, Float32) => true,
        (Float64, Int64 | Int32 | Int16 | Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        // Float32 narrowing (symmetric with Float64). Both lose the
        // fractional part; on integer-magnitude overflow the runtime
        // converter raises `TruncationForbidden` per the dispatcher.
        (Float32, Int64 | Int32 | Int16 | Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        // BigInt narrowing.
        (BigInt { .. }, BigInt { .. }) => true,
        (BigInt { .. }, Int64 | Int32 | Int16 | Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        // Decimal narrowing.
        (Decimal { .. }, Decimal { .. }) => true,
        (Decimal { .. }, BigInt { .. }) => true,
        (Decimal { .. }, Int64 | Int32 | Int16 | Int8 | UInt64 | UInt32 | UInt16 | UInt8) => true,
        // Decimal → Float64 / Float32 under truncate: the dispatcher
        // saturates magnitude overflow to `±INFINITY` and absorbs
        // mantissa rounding through the IEEE cast.
        (Decimal { .. }, Float64 | Float32) => true,
        // Json → Text(n).
        (Json, Text { size: Some(_) }) => true,
        // Xml → Text(n).
        (Xml, Text { size: Some(_) }) => true,
        // Timestamp → Date.
        (Timestamp, Date) => true,
        // Ipv6 → Ipv4: only IPv4-mapped (`::ffff:a.b.c.d`) succeeds at
        // runtime; the matrix admits the pair, dispatcher raises
        // `IpV6NotMappable` for non-mapped addresses.
        (Ipv6, Ipv4) => true,
        _ => false,
    }
}

/// Source has `digits` decimal digits (e.g. i32 → 10). Target is
/// `Decimal { precision, scale }`. Compatible if target is fully unbounded,
/// or if it has enough integer-digits room.
fn decimal_fits_int_digits(precision: Option<u32>, scale: Option<u32>, digits: u32) -> bool {
    match (precision, scale) {
        (None, _) => true,
        (Some(p), Some(s)) => p.saturating_sub(s) >= digits,
        // precision without scale is non-canonical; treat as unbounded scale=0.
        (Some(p), None) => p >= digits,
    }
}

/// Source `BigInt { width }` into `Decimal { precision, scale }`. Same idea:
/// integer-digits must cover source width.
fn decimal_fits_bigint(
    bigint_width: Option<u32>,
    precision: Option<u32>,
    scale: Option<u32>,
) -> bool {
    match (bigint_width, precision) {
        (_, None) => true,        // any → unbounded decimal: ok.
        (None, Some(_)) => false, // unbounded → bounded: rejected.
        (Some(w), Some(p)) => p.saturating_sub(scale.unwrap_or(0)) >= w,
    }
}

/// `Decimal{pa,sa} → Decimal{pb,sb}`: target unbounded, or integer-digits and
/// scale each widen.
fn decimal_fits_decimal(
    pa: Option<u32>,
    sa: Option<u32>,
    pb: Option<u32>,
    sb: Option<u32>,
) -> bool {
    match (pa, pb) {
        (_, None) => true,
        (None, Some(_)) => false,
        (Some(pa), Some(pb)) => {
            let sa = sa.unwrap_or(0);
            let sb = sb.unwrap_or(0);
            sb >= sa && pb.saturating_sub(sb) >= pa.saturating_sub(sa)
        }
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
    // Union: narrowing iff every variant is narrowing into the sink.
    if let Union(vs) = &source_t {
        return vs.iter().all(|v| is_narrowing(v.clone(), sink_t.clone()));
    }
    if matches!(sink_t, Union(_)) {
        return false;
    }
    // Custom-narrowing is opaque to the matrix — the type's own
    // `can_convert_to(_with_truncate=true)` is the source of truth.
    if matches!(&source_t, Custom(_)) || matches!(&sink_t, Custom(_)) {
        return false;
    }
    if let (Text { size: Some(a) }, Text { size: Some(b) }) = (&source_t, &sink_t) {
        return a > b;
    }
    if let (Bytes { size: Some(a) }, Bytes { size: Some(b) }) = (&source_t, &sink_t) {
        return a > b;
    }
    if let (Text { size: None }, Text { size: Some(_) }) = (&source_t, &sink_t) {
        return true;
    }
    if let (Bytes { size: None }, Bytes { size: Some(_) }) = (&source_t, &sink_t) {
        return true;
    }
    // Decimal → Float32/Float64 is always narrowing (IEEE binary
    // floats cannot exactly represent decimal fractions like `0.1`).
    // Admitted by the truncate matrix only; the validator emits
    // `NarrowingNotAllowed` so operators get the "enable truncate"
    // hint rather than `UnsupportedCast`.
    if matches!((&source_t, &sink_t), (Decimal { .. }, Float64 | Float32)) {
        return true;
    }
    matches!(
        (source_t, sink_t),
        (Int32, Int16 | Int8)
            | (Int16, Int8)
            | (Int64, Int16 | Int8)
            | (Int64, Int32)
            | (Float64, Float32)
            // Float → signed-int (truncate matrix admits all).
            | (Float32, Int8 | Int16 | Int32 | Int64)
            | (Float64, Int8 | Int16 | Int32 | Int64)
            // Float → unsigned-int (truncate matrix admits all).
            | (Float32, UInt8 | UInt16 | UInt32 | UInt64)
            | (Float64, UInt8 | UInt16 | UInt32 | UInt64)
            // Unsigned narrowing.
            | (UInt16, UInt8)
            | (UInt32, UInt8)
            | (UInt32, UInt16)
            | (UInt64, UInt8)
            | (UInt64, UInt16)
            | (UInt64, UInt32)
            // Signed → unsigned (sign loss) is also narrowing.
            | (Int8, UInt8 | UInt16 | UInt32 | UInt64)
            | (Int16, UInt8 | UInt16 | UInt32 | UInt64)
            | (Int32, UInt8 | UInt16 | UInt32 | UInt64)
            | (Int64, UInt8 | UInt16 | UInt32 | UInt64)
            // Unsigned → smaller signed also narrowing.
            | (UInt8, Int8)
            // IPv6 → IPv4: mask info loss + family narrowing.
            | (Ipv6, Ipv4)
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::any::Any;

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
    fn json_to_unbounded_text_allowed_text_to_json_rejected() {
        // Json → Text* is allowed without truncate (no truncation needed).
        assert!(is_compatible(DataType::Json, TEXT));
        // Json → Text(n) (bounded) requires truncate consent.
        assert!(!is_compatible(DataType::Json, text(100)));
        // Text → Json is not modelled.
        assert!(!is_compatible(TEXT, DataType::Json));
    }

    #[test]
    fn date_timestamp_incompatible_both_directions() {
        assert!(!is_compatible(DataType::Date, DataType::Timestamp));
        assert!(!is_compatible(DataType::Timestamp, DataType::Date));
    }

    fn bigint(w: u32) -> DataType {
        DataType::BigInt { width: Some(w) }
    }
    const BIGINT_UNB: DataType = DataType::BigInt { width: None };

    fn dec(p: u32, s: u32) -> DataType {
        DataType::Decimal {
            precision: Some(p),
            scale: Some(s),
        }
    }
    const DEC_UNB: DataType = DataType::Decimal {
        precision: None,
        scale: None,
    };

    #[test]
    fn fixed_int_into_bigint_always_ok() {
        for src in [DataType::Int16, DataType::Int32, DataType::Int64] {
            assert!(is_compatible(src.clone(), BIGINT_UNB));
            assert!(is_compatible(src, bigint(20)));
        }
    }

    #[test]
    fn bigint_widening_only() {
        assert!(is_compatible(bigint(20), BIGINT_UNB));
        assert!(is_compatible(bigint(20), bigint(40)));
        assert!(is_compatible(bigint(20), bigint(20)));
        assert!(!is_compatible(bigint(40), bigint(20)));
        assert!(!is_compatible(BIGINT_UNB, bigint(40)));
    }

    #[test]
    fn bigint_into_int_rejected() {
        assert!(!is_compatible(BIGINT_UNB, DataType::Int64));
        assert!(!is_compatible(bigint(10), DataType::Int32));
    }

    #[test]
    fn int_into_decimal_needs_integer_digits() {
        assert!(is_compatible(DataType::Int16, dec(5, 0)));
        assert!(!is_compatible(DataType::Int16, dec(4, 0)));
        assert!(is_compatible(DataType::Int32, dec(10, 0)));
        assert!(!is_compatible(DataType::Int32, dec(9, 0)));
        assert!(is_compatible(DataType::Int64, dec(19, 0)));
        assert!(!is_compatible(DataType::Int64, dec(18, 0)));
        // Widening with scale: precision − scale must still cover the int.
        assert!(is_compatible(DataType::Int32, dec(20, 10)));
        assert!(!is_compatible(DataType::Int32, dec(15, 10)));
        // Unbounded decimal soaks up everything.
        assert!(is_compatible(DataType::Int64, DEC_UNB));
    }

    #[test]
    fn bigint_into_decimal_rules() {
        assert!(is_compatible(bigint(20), dec(20, 0)));
        assert!(!is_compatible(bigint(20), dec(19, 0)));
        assert!(is_compatible(bigint(20), DEC_UNB));
        assert!(!is_compatible(BIGINT_UNB, dec(40, 0)));
        assert!(is_compatible(BIGINT_UNB, DEC_UNB));
    }

    #[test]
    fn decimal_widening_rules() {
        assert!(is_compatible(dec(10, 2), dec(12, 4))); // both grow
        assert!(is_compatible(dec(10, 2), dec(10, 2))); // identity
        assert!(is_compatible(dec(10, 2), DEC_UNB));
        assert!(!is_compatible(DEC_UNB, dec(10, 2)));
        // Scale must not shrink even if precision grows.
        assert!(!is_compatible(dec(10, 4), dec(20, 2)));
        // Integer-digits must not shrink even if scale grows.
        assert!(!is_compatible(dec(10, 2), dec(11, 4)));
    }

    #[test]
    fn decimal_into_int_or_bigint_rejected() {
        assert!(!is_compatible(dec(10, 0), DataType::Int64));
        assert!(!is_compatible(dec(10, 0), bigint(10)));
        assert!(!is_compatible(DEC_UNB, BIGINT_UNB));
    }

    #[test]
    fn unsigned_widening_within_unsigned() {
        assert!(is_compatible(DataType::UInt8, DataType::UInt16));
        assert!(is_compatible(DataType::UInt8, DataType::UInt64));
        assert!(is_compatible(DataType::UInt16, DataType::UInt32));
        assert!(is_compatible(DataType::UInt32, DataType::UInt64));
        assert!(!is_compatible(DataType::UInt32, DataType::UInt16));
        assert!(!is_compatible(DataType::UInt64, DataType::UInt32));
    }

    #[test]
    fn unsigned_widening_into_signed() {
        assert!(is_compatible(DataType::UInt8, DataType::Int16));
        assert!(is_compatible(DataType::UInt16, DataType::Int32));
        assert!(is_compatible(DataType::UInt32, DataType::Int64));
        // UInt64 max > i64 max — can't widen into any signed Int*.
        assert!(!is_compatible(DataType::UInt64, DataType::Int64));
        // Reverse: signed → unsigned is rejected (sign loss).
        assert!(!is_compatible(DataType::Int16, DataType::UInt16));
        assert!(!is_compatible(DataType::Int64, DataType::UInt64));
    }

    #[test]
    fn unsigned_into_bigint_and_decimal() {
        assert!(is_compatible(DataType::UInt8, BIGINT_UNB));
        assert!(is_compatible(DataType::UInt64, BIGINT_UNB));
        assert!(is_compatible(DataType::UInt8, dec(3, 0)));
        assert!(!is_compatible(DataType::UInt8, dec(2, 0)));
        assert!(is_compatible(DataType::UInt32, dec(10, 0)));
        assert!(!is_compatible(DataType::UInt32, dec(9, 0)));
        assert!(is_compatible(DataType::UInt64, dec(20, 0)));
        assert!(!is_compatible(DataType::UInt64, dec(19, 0)));
    }

    #[test]
    fn unsigned_to_bool_and_back() {
        assert!(is_compatible(DataType::UInt8, DataType::Bool));
        assert!(is_compatible(DataType::Bool, DataType::UInt32));
    }

    #[test]
    fn float_into_unsigned_rejected_both_directions() {
        assert!(!is_compatible(DataType::Float32, DataType::UInt32));
        assert!(!is_compatible(DataType::Float64, DataType::UInt64));
        assert!(!is_compatible(DataType::UInt32, DataType::Float32));
        assert!(!is_compatible(DataType::UInt64, DataType::Float64));
    }

    #[test]
    fn bigint_or_decimal_into_unsigned_rejected() {
        assert!(!is_compatible(BIGINT_UNB, DataType::UInt64));
        assert!(!is_compatible(bigint(10), DataType::UInt32));
        assert!(!is_compatible(dec(10, 0), DataType::UInt8));
        assert!(!is_compatible(DEC_UNB, DataType::UInt64));
    }

    #[test]
    fn unsigned_narrowing_full_matrix() {
        // Every UInt → smaller-UInt pair must be rejected and flagged narrowing.
        let pairs = [
            (DataType::UInt16, DataType::UInt8),
            (DataType::UInt32, DataType::UInt8),
            (DataType::UInt32, DataType::UInt16),
            (DataType::UInt64, DataType::UInt8),
            (DataType::UInt64, DataType::UInt16),
            (DataType::UInt64, DataType::UInt32),
        ];
        for (a, b) in pairs {
            assert!(
                !is_compatible(a.clone(), b.clone()),
                "{a:?} → {b:?} should reject"
            );
            assert!(is_narrowing(a, b), "should narrow");
        }
    }

    #[test]
    fn signed_to_unsigned_full_matrix() {
        // Sign loss in both directions: signed → unsigned never compatible.
        for src in [DataType::Int16, DataType::Int32, DataType::Int64] {
            for dst in [
                DataType::UInt8,
                DataType::UInt16,
                DataType::UInt32,
                DataType::UInt64,
            ] {
                assert!(
                    !is_compatible(src.clone(), dst.clone()),
                    "{src:?} → {dst:?} should reject"
                );
                assert!(is_narrowing(src.clone(), dst), "should narrow");
            }
        }
    }

    #[test]
    fn float_to_decimal_or_bigint_rejected() {
        // Float → fixed-point (Decimal / BigInt) rejected by the lossless
        // matrix in both directions — runtime semantics for the cast are
        // ambiguous (round vs truncate vs error). Wide / unbounded
        // Decimal → Float is also rejected losslessly; see
        // `decimal_narrow_to_float_lossless_only_when_precision_fits`
        // for the bounded-narrow-precision allow path.
        assert!(!is_compatible(DataType::Float32, dec(10, 2)));
        assert!(!is_compatible(DataType::Float64, dec(20, 4)));
        assert!(!is_compatible(DataType::Float32, BIGINT_UNB));
        assert!(!is_compatible(BIGINT_UNB, DataType::Float64));
    }

    #[test]
    fn decimal_to_float_is_never_lossless() {
        // Even at precision ≤ 15, fractional decimals like `0.1` have
        // no exact IEEE-754 binary representation. Every Decimal →
        // Float pair must require `truncate = true`.
        assert!(!is_compatible(dec(12, 2), DataType::Float64));
        assert!(!is_compatible(dec(15, 0), DataType::Float64));
        assert!(!is_compatible(dec(16, 0), DataType::Float64));
        assert!(!is_compatible(DEC_UNB, DataType::Float64));

        assert!(!is_compatible(dec(7, 2), DataType::Float32));
        assert!(!is_compatible(dec(8, 2), DataType::Float32));
        assert!(!is_compatible(DEC_UNB, DataType::Float32));
    }

    #[test]
    fn decimal_to_float_unlocks_under_truncate() {
        // The truncate matrix admits every Decimal → Float pair.
        // The runtime dispatcher saturates magnitude overflow to
        // ±INFINITY and absorbs mantissa loss through the IEEE cast.
        assert_unlocks(dec(12, 2), DataType::Float64);
        assert_unlocks(dec(38, 0), DataType::Float64);
        assert_unlocks(DEC_UNB, DataType::Float64);
        assert_unlocks(dec(7, 2), DataType::Float32);
        assert_unlocks(dec(38, 0), DataType::Float32);
        assert_unlocks(DEC_UNB, DataType::Float32);
    }

    #[test]
    fn decimal_to_float_is_always_narrowing() {
        // Every Decimal → Float pair is narrowing (admitted by
        // truncate matrix, rejected losslessly) so the validator
        // emits `NarrowingNotAllowed` with the "enable truncate" hint
        // rather than `UnsupportedCast`.
        assert!(is_narrowing(dec(12, 2), DataType::Float64));
        assert!(is_narrowing(dec(38, 0), DataType::Float64));
        assert!(is_narrowing(DEC_UNB, DataType::Float64));
        assert!(is_narrowing(dec(7, 2), DataType::Float32));
        assert!(is_narrowing(dec(8, 2), DataType::Float32));
    }

    // ---- Xml + truncate matrix coverage ----

    #[test]
    fn xml_identity_and_unbounded_text() {
        assert!(is_compatible(DataType::Xml, DataType::Xml));
        assert!(is_compatible(DataType::Xml, TEXT));
        // Bounded sink without truncate is rejected.
        assert!(!is_compatible(DataType::Xml, text(100)));
    }

    #[test]
    fn text_to_xml_compatible() {
        assert!(is_compatible(text(36), DataType::Xml));
        assert!(is_compatible(TEXT, DataType::Xml));
    }

    #[test]
    fn text_to_bool_compatible() {
        assert!(is_compatible(text(10), DataType::Bool));
        assert!(is_compatible(TEXT, DataType::Bool));
    }

    #[test]
    fn truncate_unlocks_text_narrow() {
        assert!(!is_compatible(text(20), text(10)));
        assert!(is_compatible_with_truncate(text(20), text(10)));
        // Unbounded → bounded gated as well.
        assert!(!is_compatible(TEXT, text(10)));
        assert!(is_compatible_with_truncate(TEXT, text(10)));
    }

    #[test]
    fn truncate_unlocks_int_narrow_and_sign_loss() {
        assert!(!is_compatible(DataType::Int64, DataType::Int32));
        assert!(is_compatible_with_truncate(
            DataType::Int64,
            DataType::Int32
        ));
        assert!(!is_compatible(DataType::Int32, DataType::UInt32));
        assert!(is_compatible_with_truncate(
            DataType::Int32,
            DataType::UInt32
        ));
    }

    #[test]
    fn truncate_unlocks_float_to_int() {
        assert!(!is_compatible(DataType::Float64, DataType::Int32));
        assert!(is_compatible_with_truncate(
            DataType::Float64,
            DataType::Int32
        ));
    }

    #[test]
    fn truncate_unlocks_decimal_to_bigint_to_int() {
        assert!(!is_compatible(dec(10, 2), BIGINT_UNB));
        assert!(is_compatible_with_truncate(dec(10, 2), BIGINT_UNB));
        assert!(!is_compatible(BIGINT_UNB, DataType::Int32));
        assert!(is_compatible_with_truncate(BIGINT_UNB, DataType::Int32));
    }

    #[test]
    fn truncate_unlocks_json_xml_to_bounded_text() {
        assert!(!is_compatible(DataType::Json, text(100)));
        assert!(is_compatible_with_truncate(DataType::Json, text(100)));
        assert!(!is_compatible(DataType::Xml, text(100)));
        assert!(is_compatible_with_truncate(DataType::Xml, text(100)));
    }

    #[test]
    fn truncate_unlocks_timestamp_to_date() {
        assert!(!is_compatible(DataType::Timestamp, DataType::Date));
        assert!(is_compatible_with_truncate(
            DataType::Timestamp,
            DataType::Date
        ));
    }

    #[test]
    fn truncate_allows_json_xml_identity() {
        // Identity flows through the super-relation: `truncate=true` on a
        // lossless `Json→Json` / `Xml→Xml` mapping is a harmless no-op,
        // never a request to actually truncate the structured payload.
        // The runtime dispatcher is the place that raises
        // `TruncationForbidden` if truncation is ever actually attempted.
        assert!(is_compatible(DataType::Json, DataType::Json));
        assert!(is_compatible_with_truncate(DataType::Json, DataType::Json));
        assert!(is_compatible(DataType::Xml, DataType::Xml));
        assert!(is_compatible_with_truncate(DataType::Xml, DataType::Xml));
    }

    #[test]
    fn truncate_forbids_uuid_short_text_or_bytes() {
        assert!(!is_compatible_with_truncate(DataType::Uuid, text(35)));
        assert!(!is_compatible_with_truncate(DataType::Uuid, bytes(15)));
    }

    #[test]
    fn truncate_allows_atomic_identities() {
        // Atomic identity-with-truncate flows through the super-relation:
        // `truncate=true` on a lossless identity is a harmless no-op. The
        // motivating case is a Mongo `Timestamp → Date with truncate=true`
        // mapping whose `plan.sink = Date` is then re-validated against a
        // `Date` sink column; the super-relation must admit it.
        assert!(is_compatible_with_truncate(DataType::Uuid, DataType::Uuid));
        assert!(is_compatible_with_truncate(DataType::Date, DataType::Date));
        assert!(is_compatible_with_truncate(
            DataType::Timestamp,
            DataType::Timestamp
        ));
    }

    #[test]
    fn truncate_does_not_unlock_date_to_timestamp() {
        assert!(!is_compatible_with_truncate(
            DataType::Date,
            DataType::Timestamp
        ));
    }

    // ---- Truncate allow-list — exhaustive walk ---------------------

    /// Two-sided invariant for every "unlock" pair: rejected without
    /// `truncate`, allowed with `truncate`. Asserting both sides catches a
    /// regression where `is_compatible_with_truncate` returned `true`
    /// unconditionally (which a one-sided walk would silently accept).
    fn assert_unlocks(a: DataType, b: DataType) {
        assert!(
            !is_compatible(a.clone(), b.clone()),
            "{a:?} → {b:?} should reject lossless"
        );
        assert!(
            is_compatible_with_truncate(a, b),
            "should unlock with truncate"
        );
    }

    #[test]
    fn truncate_unlocks_full_signed_narrow_matrix() {
        for (a, b) in [
            (DataType::Int64, DataType::Int32),
            (DataType::Int64, DataType::Int16),
            (DataType::Int32, DataType::Int16),
        ] {
            assert_unlocks(a, b);
        }
    }

    #[test]
    fn truncate_unlocks_full_unsigned_narrow_matrix() {
        for (a, b) in [
            (DataType::UInt64, DataType::UInt32),
            (DataType::UInt64, DataType::UInt16),
            (DataType::UInt64, DataType::UInt8),
            (DataType::UInt32, DataType::UInt16),
            (DataType::UInt32, DataType::UInt8),
            (DataType::UInt16, DataType::UInt8),
        ] {
            assert_unlocks(a, b);
        }
    }

    #[test]
    fn truncate_unlocks_full_signed_to_unsigned_matrix() {
        for s in [DataType::Int16, DataType::Int32, DataType::Int64] {
            for u in [
                DataType::UInt8,
                DataType::UInt16,
                DataType::UInt32,
                DataType::UInt64,
            ] {
                assert_unlocks(s.clone(), u);
            }
        }
    }

    #[test]
    fn truncate_unlocks_full_unsigned_to_signed_matrix() {
        for (a, b) in [
            (DataType::UInt64, DataType::Int64),
            (DataType::UInt64, DataType::Int32),
            (DataType::UInt64, DataType::Int16),
            (DataType::UInt32, DataType::Int32),
            (DataType::UInt32, DataType::Int16),
            (DataType::UInt16, DataType::Int16),
        ] {
            assert_unlocks(a, b);
        }
    }

    #[test]
    fn truncate_unlocks_float64_to_all_int_widths() {
        for d in [
            DataType::Float32,
            DataType::Int64,
            DataType::Int32,
            DataType::Int16,
            DataType::UInt64,
            DataType::UInt32,
            DataType::UInt16,
            DataType::UInt8,
        ] {
            assert_unlocks(DataType::Float64, d);
        }
    }

    #[test]
    fn truncate_unlocks_decimal_to_all_int_widths() {
        for d in [
            DataType::Int64,
            DataType::Int32,
            DataType::Int16,
            DataType::UInt64,
            DataType::UInt32,
            DataType::UInt16,
            DataType::UInt8,
            BIGINT_UNB,
        ] {
            assert_unlocks(dec(10, 2), d);
        }
    }

    #[test]
    fn truncate_unlocks_bigint_to_all_int_widths() {
        for d in [
            DataType::Int64,
            DataType::Int32,
            DataType::Int16,
            DataType::UInt64,
            DataType::UInt32,
            DataType::UInt16,
            DataType::UInt8,
        ] {
            assert_unlocks(BIGINT_UNB, d);
        }
    }

    #[test]
    fn truncate_unlocks_decimal_narrow_each_dimension() {
        // Both dimensions shrink simultaneously.
        assert_unlocks(dec(20, 4), dec(10, 2));
        // Precision-only shrink.
        assert_unlocks(dec(20, 2), dec(10, 2));
        // Scale-only shrink.
        assert_unlocks(dec(20, 4), dec(20, 2));
    }

    #[test]
    fn truncate_unlocks_bytes_narrow_with_unbounded_source() {
        assert!(!is_compatible(BYTES, bytes(10)));
        assert!(is_compatible_with_truncate(BYTES, bytes(10)));
    }

    /// Super-relation invariant: every identity pair `(T, T)` that
    /// `is_compatible` admits must also be admitted by
    /// `is_compatible_with_truncate`. This guards against regressions
    /// where the truncate matrix drifts back into a disjoint relation
    /// and a `truncate=true` flag on an already-lossless identity is
    /// wrongly rejected at validation time (Mongo `Timestamp → Date
    /// with truncate=true` would re-check `(Date, Date)` and fail).
    /// `Union` and `Custom` are skipped — both have variant-dependent
    /// semantics and are covered by their own dedicated tests.
    #[test]
    fn truncate_admits_every_identity() {
        let variants: Vec<DataType> = vec![
            DataType::Bool,
            DataType::Int8,
            DataType::Int16,
            DataType::Int32,
            DataType::Int64,
            DataType::UInt8,
            DataType::UInt16,
            DataType::UInt32,
            DataType::UInt64,
            DataType::Float32,
            DataType::Float64,
            bigint(20),
            dec(10, 4),
            TEXT,
            text(64),
            BYTES,
            bytes(16),
            DataType::Date,
            DataType::Timestamp,
            DataType::Uuid,
            DataType::Ipv4,
            DataType::Ipv6,
            DataType::Json,
            DataType::Xml,
        ];
        for t in variants {
            assert!(
                is_compatible(t.clone(), t.clone()),
                "{t:?} must be lossless-compatible with itself"
            );
            assert!(
                is_compatible_with_truncate(t.clone(), t.clone()),
                "{t:?} must remain compatible under truncate (super-relation)"
            );
        }
    }

    // ---- Truncate deny-list under truncate -------------------------

    #[test]
    fn truncate_does_not_unlock_unrelated_pairs() {
        // Unrelated pairs remain incompatible even with truncate.
        assert!(!is_compatible_with_truncate(DataType::Bool, DataType::Date));
        assert!(!is_compatible_with_truncate(DataType::Date, DataType::Json));
        assert!(!is_compatible_with_truncate(
            DataType::Uuid,
            DataType::Timestamp
        ));
    }

    #[test]
    fn truncate_uuid_to_wide_text_or_bytes_still_ok() {
        // Truncation is a no-op when the source already fits — fall through to is_compatible.
        assert!(is_compatible_with_truncate(DataType::Uuid, text(36)));
        assert!(is_compatible_with_truncate(DataType::Uuid, bytes(16)));
    }

    // ---- is_narrowing() coverage gaps ------------------------------

    #[test]
    fn narrowing_text_unbounded_to_bounded() {
        assert!(is_narrowing(TEXT, text(10)));
        assert!(is_narrowing(BYTES, bytes(10)));
    }

    #[test]
    fn narrowing_text_widening_returns_false() {
        assert!(!is_narrowing(text(10), text(20)));
        assert!(!is_narrowing(text(10), TEXT));
    }

    #[test]
    fn narrowing_distinct_unrelated_pair_returns_false() {
        assert!(!is_narrowing(DataType::Json, DataType::Bool));
        assert!(!is_narrowing(DataType::Date, DataType::Timestamp));
    }

    // ---- Union (Mongo heterogeneous source field) ------------------

    #[test]
    fn union_src_compatible_when_every_variant_is() {
        // Both Int16 and Int32 widen losslessly into Int64.
        let src = DataType::union(vec![DataType::Int16, DataType::Int32]);
        assert!(is_compatible(src, DataType::Int64));
    }

    #[test]
    fn union_src_rejected_when_any_variant_incompatible() {
        // Int32 is compatible with Int64; Text is not. The union as a
        // whole must be rejected against an Int64 sink.
        let src = DataType::union(vec![DataType::Int32, DataType::Text { size: None }]);
        assert!(!is_compatible(src, DataType::Int64));
    }

    #[test]
    fn union_sink_always_rejected() {
        // Sinks never carry Union — the matrix must reject it
        // unconditionally so a misconfigured pipeline surfaces at
        // validate time.
        let sink = DataType::union(vec![DataType::Int32, DataType::Text { size: None }]);
        assert!(!is_compatible(DataType::Int32, sink.clone()));
        assert!(!is_compatible_with_truncate(DataType::Int32, sink));
    }

    #[test]
    fn union_src_with_truncate_rejected_when_any_arm_lacks_truncate_path() {
        // `Int32 → Text(10)` is not in the truncate matrix, so a union
        // containing Int32 cannot land in a Text sink even with
        // truncate=true.
        let src = DataType::union(vec![DataType::Int32, DataType::Text { size: None }]);
        assert!(!is_compatible(src.clone(), text(10)));
        assert!(!is_compatible_with_truncate(src, text(10)));
    }

    #[test]
    fn union_src_with_truncate_unlocks_when_every_arm_unlocks() {
        // Both `Text(unbounded) → Text(10)` and `Text(20) → Text(10)`
        // are unlocked by truncate. Combined as a union they unlock
        // together — the matrix walks every member.
        let src = DataType::union(vec![DataType::Text { size: None }, text(20)]);
        assert!(!is_compatible(src.clone(), text(10)));
        assert!(is_compatible_with_truncate(src, text(10)));
    }

    #[test]
    fn union_singleton_collapses_to_concrete() {
        // `union([T])` is normalised to bare `T` by the constructor —
        // matrix sees a concrete type, not a Union wrapper.
        let dt = DataType::union(vec![DataType::Int32]);
        assert_eq!(dt, DataType::Int32);
    }

    #[test]
    fn union_flattens_nested_inputs() {
        // Union members that are themselves Union must be flattened —
        // the matrix and dispatcher rely on a one-level-deep invariant.
        let inner = DataType::union(vec![DataType::Int32, DataType::Text { size: None }]);
        let outer = DataType::union(vec![inner, DataType::Int64]);
        match &outer {
            DataType::Union(vs) => {
                assert_eq!(vs.len(), 3, "expected 3 flat variants, got {vs:?}");
                assert!(vs.contains(&DataType::Int32));
                assert!(vs.contains(&DataType::Int64));
                assert!(vs.contains(&DataType::Text { size: None }));
                assert!(
                    !vs.iter().any(|v| matches!(v, DataType::Union(_))),
                    "nested Union must be flattened"
                );
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn union_dedup_and_sort_normalises() {
        // Equality must be observation-order-independent so two flows
        // that saw `int+text` vs `text+int` produce identical schemas.
        let a = DataType::union(vec![DataType::Int32, DataType::Text { size: None }]);
        let b = DataType::union(vec![DataType::Text { size: None }, DataType::Int32]);
        assert_eq!(a, b);
        // Duplicates are collapsed.
        let c = DataType::union(vec![DataType::Int32, DataType::Int32]);
        assert_eq!(c, DataType::Int32);
    }

    // ---- Custom routing -------------------------------------------

    use crate::types::convert::ConvertError;
    use crate::types::convert::context::ConversionContext;
    use crate::types::dynamic::DynType;
    use crate::types::value::Value;

    /// Test type that converts to/from `Bytes { size: None }` only.
    #[derive(Debug)]
    struct BytesyType;

    impl DynType for BytesyType {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.bytesy"
        }
        fn can_convert_to(&self, target: &DataType, _truncate: bool) -> bool {
            matches!(target, DataType::Bytes { size: None })
        }
        fn can_construct_from(&self, src: &DataType, _truncate: bool) -> bool {
            matches!(src, DataType::Bytes { size: None })
        }
        fn convert(
            &self,
            v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            Ok(v)
        }
        fn construct(
            &self,
            v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            Ok(v)
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(BytesyType)
        }
    }

    #[test]
    fn custom_to_bytes_via_can_convert_to() {
        let src = DataType::Custom(Box::new(BytesyType));
        assert!(is_compatible(src.clone(), DataType::Bytes { size: None }));
        assert!(is_compatible_with_truncate(
            src,
            DataType::Bytes { size: None }
        ));
    }

    #[test]
    fn custom_to_canonical_rejects_unsupported_target() {
        let src = DataType::Custom(Box::new(BytesyType));
        assert!(!is_compatible(src.clone(), DataType::Int32));
        assert!(!is_compatible_with_truncate(src, DataType::Int32));
    }

    #[test]
    fn canonical_into_custom_via_can_construct_from() {
        let dst = DataType::Custom(Box::new(BytesyType));
        assert!(is_compatible(DataType::Bytes { size: None }, dst.clone()));
        assert!(is_compatible_with_truncate(
            DataType::Bytes { size: None },
            dst
        ));
    }

    #[test]
    fn custom_to_custom_identity() {
        let a = DataType::Custom(Box::new(BytesyType));
        let b = DataType::Custom(Box::new(BytesyType));
        assert!(is_compatible(a.clone(), b.clone()));
        assert!(is_compatible_with_truncate(a, b));
    }

    #[test]
    fn is_narrowing_returns_false_for_custom() {
        let custom = DataType::Custom(Box::new(BytesyType));
        assert!(!is_narrowing(
            custom.clone(),
            DataType::Bytes { size: None }
        ));
        assert!(!is_narrowing(DataType::Bytes { size: None }, custom));
    }

    /// Float32 → Int/UInt narrowing is admitted **only** under
    /// `truncate=true` (symmetric with Float64 → Int/UInt). The lossless
    /// matrix rejects, the truncate matrix admits, and `is_narrowing`
    /// reports `true` so the validator surfaces `NarrowingNotAllowed`
    /// (with the "enable truncate" hint) — not `UnsupportedCast`.
    // ---- IPv4 / IPv6 matrix coverage ------------------------------

    #[test]
    fn ip_identity_compatible() {
        assert!(is_compatible(DataType::Ipv4, DataType::Ipv4));
        assert!(is_compatible(DataType::Ipv6, DataType::Ipv6));
    }

    #[test]
    fn ip_widening_v4_to_v6_lossless() {
        // IPv4 → IPv6 is always lossless via IPv4-mapped form
        // (::ffff:a.b.c.d).
        assert!(is_compatible(DataType::Ipv4, DataType::Ipv6));
    }

    #[test]
    fn ip_narrowing_v6_to_v4_only_under_truncate() {
        // The lossless matrix rejects v6 → v4 because most v6 cells
        // are not extractable. truncate=true unlocks the runtime
        // IPv4-mapped check (dispatcher raises IpV6NotMappable for
        // non-mapped addresses).
        assert!(!is_compatible(DataType::Ipv6, DataType::Ipv4));
        assert!(is_compatible_with_truncate(DataType::Ipv6, DataType::Ipv4));
        assert!(is_narrowing(DataType::Ipv6, DataType::Ipv4));
    }

    #[test]
    fn ip_to_text_requires_canonical_width() {
        // IPv4 canonical max = "255.255.255.255" = 15 chars.
        assert!(is_compatible(DataType::Ipv4, text(15)));
        assert!(is_compatible(DataType::Ipv4, text(20)));
        assert!(is_compatible(DataType::Ipv4, TEXT));
        assert!(!is_compatible(DataType::Ipv4, text(14)));
        // IPv6 RFC 5952 canonical max = 39 chars.
        assert!(is_compatible(DataType::Ipv6, text(39)));
        assert!(is_compatible(DataType::Ipv6, text(45)));
        assert!(is_compatible(DataType::Ipv6, TEXT));
        assert!(!is_compatible(DataType::Ipv6, text(38)));
    }

    #[test]
    fn text_to_ip_admit_liberal_parser_validates_at_runtime() {
        // Text → IP admits liberally; the parser raises InvalidIp at
        // runtime for malformed addresses. Sizes 7 (v4 minimum
        // "0.0.0.0") and 2 (v6 minimum "::") are the floor.
        assert!(is_compatible(text(7), DataType::Ipv4));
        assert!(is_compatible(TEXT, DataType::Ipv4));
        assert!(!is_compatible(text(6), DataType::Ipv4));
        assert!(is_compatible(text(2), DataType::Ipv6));
        assert!(is_compatible(TEXT, DataType::Ipv6));
        assert!(!is_compatible(text(1), DataType::Ipv6));
    }

    #[test]
    fn ip_to_bytes_network_order_widths() {
        // Network byte order (BE) octets per RFC 791/8200 —
        // v4 = 4 bytes, v6 = 16 bytes.
        assert!(is_compatible(DataType::Ipv4, bytes(4)));
        assert!(is_compatible(DataType::Ipv4, bytes(16)));
        assert!(is_compatible(DataType::Ipv4, BYTES));
        assert!(!is_compatible(DataType::Ipv4, bytes(3)));
        assert!(is_compatible(DataType::Ipv6, bytes(16)));
        assert!(is_compatible(DataType::Ipv6, BYTES));
        assert!(!is_compatible(DataType::Ipv6, bytes(15)));
    }

    #[test]
    fn bytes_to_ip_admit_with_minimum_width() {
        assert!(is_compatible(bytes(4), DataType::Ipv4));
        assert!(is_compatible(BYTES, DataType::Ipv4));
        assert!(!is_compatible(bytes(3), DataType::Ipv4));
        assert!(is_compatible(bytes(16), DataType::Ipv6));
        assert!(is_compatible(BYTES, DataType::Ipv6));
        assert!(!is_compatible(bytes(15), DataType::Ipv6));
    }

    #[test]
    fn ip_rejected_against_unrelated_canonical_types() {
        for unrelated in [
            DataType::Int32,
            DataType::Int64,
            DataType::Float64,
            DataType::Bool,
            DataType::Date,
            DataType::Timestamp,
            DataType::Uuid,
            DataType::Json,
            DataType::Xml,
            dec(10, 2),
            bigint(20),
        ] {
            assert!(
                !is_compatible(DataType::Ipv4, unrelated.clone()),
                "Ipv4 → {unrelated:?} should reject"
            );
            assert!(
                !is_compatible(DataType::Ipv6, unrelated.clone()),
                "Ipv6 → {unrelated:?} should reject"
            );
            assert!(
                !is_compatible(unrelated.clone(), DataType::Ipv4),
                "{unrelated:?} → Ipv4 should reject"
            );
            assert!(!is_compatible(unrelated, DataType::Ipv6), "should reject");
        }
    }

    #[test]
    fn truncate_admits_ip_identity_and_widening() {
        assert!(is_compatible_with_truncate(DataType::Ipv4, DataType::Ipv4));
        assert!(is_compatible_with_truncate(DataType::Ipv6, DataType::Ipv6));
        assert!(is_compatible_with_truncate(DataType::Ipv4, DataType::Ipv6));
    }

    #[test]
    fn float32_to_int_admitted_under_truncate() {
        let sinks = [
            DataType::Int8,
            DataType::Int16,
            DataType::Int32,
            DataType::Int64,
            DataType::UInt8,
            DataType::UInt16,
            DataType::UInt32,
            DataType::UInt64,
        ];
        for sink in sinks {
            assert!(
                !is_compatible(DataType::Float32, sink.clone()),
                "Float32 → {sink:?} must be rejected by the lossless matrix"
            );
            assert!(
                is_compatible_with_truncate(DataType::Float32, sink.clone()),
                "Float32 → {sink:?} must be admitted by the truncate matrix"
            );
            assert!(
                is_narrowing(DataType::Float32, sink.clone()),
                "Float32 → {sink:?} must report as narrowing so the validator \
                 emits NarrowingNotAllowed when truncate is missing"
            );
        }
    }
}
