use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use num_bigint::BigInt;
use num_traits::ToPrimitive;
use smallvec::SmallVec;

use crate::Value;
use crate::error::KeyError;

/// Hashable, totally-ordered projection of [`Value`] for switch dispatch,
/// batch dedup, and cursor comparison.
///
/// Internal storage uses `SmallVec<[Value; 2]>` — single and two-element
/// keys live inline on the stack; three or more spill to the heap.
///
/// Construction validates types (rejects Null/Json/Object, and Custom
/// unless `cursor_compatible`) and canonicalises the representation:
/// small ints promote to Int64, Float32 widens to Float64, UInt64
/// either fits Int64 or promotes to BigInt.
///
/// Unlike [`Value`], `Key` implements total ordering (`Ord`) and total
/// equality (`Eq`): NaN == NaN by design so keys are deterministic.
#[derive(Debug, Clone)]
pub struct Key {
    values: SmallVec<[Value; 2]>,
}

impl Key {
    /// Create a single-element key.
    pub fn single(value: Value) -> Result<Self, KeyError> {
        let canonical = canonicalise(value)?;
        let mut values = SmallVec::new();
        values.push(canonical);
        Ok(Key { values })
    }

    /// Create a composite (multi-element) key.
    pub fn composite(raw: Vec<Value>) -> Result<Self, KeyError> {
        if raw.is_empty() {
            return Err(KeyError::EmptyComposite);
        }
        let mut values = SmallVec::with_capacity(raw.len());
        for v in raw {
            values.push(canonicalise(v)?);
        }
        Ok(Key { values })
    }

    /// Runtime shortcut: project a [`Value`] into a single-element key.
    /// Returns `None` for unsupported types (Null, Json, Object, non-cursor Custom).
    pub fn from_value(v: &Value) -> Option<Self> {
        Self::single(v.clone()).ok()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn values(&self) -> &[Value] {
        &self.values
    }

    /// Unwrap a single-element key back into its Value.
    pub fn into_single(self) -> Option<Value> {
        if self.values.len() == 1 {
            Some(self.values.into_iter().next().expect("len == 1"))
        } else {
            None
        }
    }
}

fn canonicalise(value: Value) -> Result<Value, KeyError> {
    match value {
        Value::Null => Err(KeyError::UnsupportedType("Null")),
        Value::Json(_) => Err(KeyError::UnsupportedType("Json")),
        Value::Object(_) => Err(KeyError::UnsupportedType("Object")),
        Value::Interval(_) => Err(KeyError::UnsupportedType("Interval")),
        Value::Custom(ref c) => {
            if c.dyn_type().cursor_compatible() {
                Ok(value)
            } else {
                Err(KeyError::UnsupportedType("Custom (not cursor-compatible)"))
            }
        }
        Value::Int8(n) => Ok(Value::Int64(i64::from(n))),
        Value::Int16(n) => Ok(Value::Int64(i64::from(n))),
        Value::Int32(n) => Ok(Value::Int64(i64::from(n))),
        Value::UInt8(n) => Ok(Value::Int64(i64::from(n))),
        Value::UInt16(n) => Ok(Value::Int64(i64::from(n))),
        Value::UInt32(n) => Ok(Value::Int64(i64::from(n))),
        Value::UInt64(n) => match i64::try_from(n) {
            Ok(i) => Ok(Value::Int64(i)),
            Err(_) => Ok(Value::BigInt(BigInt::from(n))),
        },
        Value::Float32(f) => Ok(Value::Float64(f64::from(f))),
        v @ (Value::Bool(_)
        | Value::Int64(_)
        | Value::BigInt(_)
        | Value::Float64(_)
        | Value::Decimal(_)
        | Value::Text(_)
        | Value::Bytes(_)
        | Value::Date(_)
        | Value::Timestamp(_)
        | Value::Uuid(_)
        | Value::Ipv4(_)
        | Value::Ipv6(_)) => Ok(v),
    }
}

fn normalise_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else if f == 0.0 {
        0u64
    } else {
        f.to_bits()
    }
}

fn hash_element<H: Hasher>(value: &Value, state: &mut H) {
    match value {
        // Numeric family: `Int64`, `BigInt`, and `Float64` share one tag and
        // hash by their `f64` value, so values that `eq_element`/`compare_values`
        // treat as equal across these types (e.g. `Int64(1)` and `Float64(1.0)`,
        // or `Int64(n)` and `BigInt(n)`) land in the same bucket — the Eq/Hash
        // contract. The coercion is deliberately lossy in the same way the
        // cross-numeric comparison is, so two distinct large integers may share
        // a bucket (a harmless collision resolved by `eq_element`).
        Value::Int64(n) => {
            0u8.hash(state);
            normalise_float_bits(*n as f64).hash(state);
        }
        Value::BigInt(b) => {
            0u8.hash(state);
            normalise_float_bits(b.to_f64().unwrap_or(f64::NAN)).hash(state);
        }
        Value::Float64(f) => {
            0u8.hash(state);
            normalise_float_bits(*f).hash(state);
        }
        Value::Bool(b) => {
            1u8.hash(state);
            b.hash(state);
        }
        Value::Text(s) => {
            3u8.hash(state);
            s.hash(state);
        }
        Value::Bytes(b) => {
            4u8.hash(state);
            b.hash(state);
        }
        Value::Date(d) => {
            5u8.hash(state);
            d.hash(state);
        }
        Value::Timestamp(t) => {
            6u8.hash(state);
            t.hash(state);
        }
        Value::Uuid(u) => {
            7u8.hash(state);
            u.hash(state);
        }
        Value::Decimal(d) => {
            8u8.hash(state);
            d.normalized().to_string().hash(state);
        }
        // IP family: `Ipv4` and `Ipv6` share one tag and hash by the v6-mapped
        // form, matching the cross-IP equality in `compare_values` (an IPv4 and
        // its IPv4-mapped IPv6 compare equal).
        Value::Ipv4(a) => {
            9u8.hash(state);
            a.to_ipv6_mapped().octets().hash(state);
        }
        Value::Ipv6(a) => {
            9u8.hash(state);
            a.octets().hash(state);
        }
        Value::Custom(c) => {
            11u8.hash(state);
            c.dyn_type().kind().hash(state);
            c.hash(state);
        }
        _ => unreachable!("canonicalise rejects this variant"),
    }
}

impl Hash for Key {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.values.len().hash(state);
        for v in &self.values {
            hash_element(v, state);
        }
    }
}

fn type_order(v: &Value) -> u8 {
    match v {
        Value::Bool(_) => 0,
        Value::Int64(_) | Value::BigInt(_) => 1,
        Value::Float64(_) => 2,
        Value::Decimal(_) => 3,
        Value::Text(_) => 4,
        Value::Bytes(_) => 5,
        Value::Date(_) => 6,
        Value::Timestamp(_) => 7,
        Value::Uuid(_) => 8,
        Value::Ipv4(_) | Value::Ipv6(_) => 9,
        Value::Custom(_) => 10,
        _ => 255,
    }
}

/// Total ordering via `compare_values` with NaN fallback.
/// NaN sorts after all non-NaN floats; cross-type falls back to `type_order`.
fn cmp_element(a: &Value, b: &Value) -> Ordering {
    if let Some(ord) = crate::compare::compare_values(a, b) {
        return ord;
    }
    // compare_values returns None for NaN or cross-type.
    // NaN handling: make NaN == NaN and NaN > non-NaN.
    match (a, b) {
        (Value::Float64(fa), Value::Float64(fb)) => {
            match (fa.is_nan(), fb.is_nan()) {
                (true, true) => Ordering::Equal,
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => Ordering::Equal, // both non-NaN but compare_values returned None — shouldn't happen
            }
        }
        _ => type_order(a).cmp(&type_order(b)),
    }
}

/// Total equality via `compare_values` with NaN == NaN.
fn eq_element(a: &Value, b: &Value) -> bool {
    if let Some(ord) = crate::compare::compare_values(a, b) {
        return ord == Ordering::Equal;
    }
    // NaN == NaN for key determinism.
    matches!((a, b), (Value::Float64(fa), Value::Float64(fb)) if fa.is_nan() && fb.is_nan())
}

impl PartialEq for Key {
    fn eq(&self, other: &Self) -> bool {
        if self.values.len() != other.values.len() {
            return false;
        }
        self.values
            .iter()
            .zip(other.values.iter())
            .all(|(a, b)| eq_element(a, b))
    }
}

impl Eq for Key {}

impl PartialOrd for Key {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Key {
    fn cmp(&self, other: &Self) -> Ordering {
        for (a, b) in self.values.iter().zip(other.values.iter()) {
            let ord = cmp_element(a, b);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        self.values.len().cmp(&other.values.len())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use std::collections::hash_map::DefaultHasher;
    use std::str::FromStr;

    fn hash_of(key: &Key) -> u64 {
        let mut h = DefaultHasher::new();
        key.hash(&mut h);
        h.finish()
    }

    #[test]
    fn small_ints_canonicalise_to_int64() {
        let k8 = Key::single(Value::Int8(42)).unwrap();
        let k64 = Key::single(Value::Int64(42)).unwrap();
        assert_eq!(k8, k64);
        assert_eq!(hash_of(&k8), hash_of(&k64));
    }

    #[test]
    fn uint_widths_canonicalise() {
        let ku16 = Key::single(Value::UInt16(1000)).unwrap();
        let k64 = Key::single(Value::Int64(1000)).unwrap();
        assert_eq!(ku16, k64);
        assert_eq!(hash_of(&ku16), hash_of(&k64));
    }

    #[test]
    fn uint64_large_promotes_to_bigint() {
        let val = u64::MAX;
        let k = Key::single(Value::UInt64(val)).unwrap();
        assert_eq!(k.values()[0], Value::BigInt(BigInt::from(val)));
    }

    #[test]
    fn bigint_int64_cross_arm_equality() {
        let k_int = Key::single(Value::Int64(42)).unwrap();
        let k_big = Key::single(Value::BigInt(BigInt::from(42))).unwrap();
        assert_eq!(k_int, k_big);
        assert_eq!(hash_of(&k_int), hash_of(&k_big));
    }

    #[test]
    fn nan_equals_nan_in_key() {
        let k1 = Key::single(Value::Float64(f64::NAN)).unwrap();
        let k2 = Key::single(Value::Float64(f64::NAN)).unwrap();
        assert_eq!(k1, k2);
        assert_eq!(hash_of(&k1), hash_of(&k2));
    }

    #[test]
    fn neg_zero_equals_pos_zero() {
        let kp = Key::single(Value::Float64(0.0)).unwrap();
        let kn = Key::single(Value::Float64(-0.0)).unwrap();
        assert_eq!(kp, kn);
        assert_eq!(hash_of(&kp), hash_of(&kn));
    }

    #[test]
    fn float32_widens_to_float64() {
        let k32 = Key::single(Value::Float32(1.5)).unwrap();
        let k64 = Key::single(Value::Float64(1.5)).unwrap();
        assert_eq!(k32, k64);
        assert_eq!(hash_of(&k32), hash_of(&k64));
    }

    #[test]
    fn int_and_equal_float_share_hash() {
        // Cross-numeric equality must imply equal hashes, else a HashMap keyed
        // by `Key` would miss a float entry on an integer-equal lookup.
        let k_int = Key::single(Value::Int64(1)).unwrap();
        let k_float = Key::single(Value::Float64(1.0)).unwrap();
        assert_eq!(k_int, k_float);
        assert_eq!(hash_of(&k_int), hash_of(&k_float));
        // A fractional float is a distinct key.
        assert_ne!(k_int, Key::single(Value::Float64(1.5)).unwrap());
    }

    #[test]
    fn lossy_large_int_and_float_share_hash() {
        // `2^53 + 1` is not exactly representable in f64 and the cross-numeric
        // comparison rounds it to `2^53.0`, so these compare equal — the Eq/Hash
        // contract then requires identical hashes (the corner case a property
        // test shrank to before the hash was made f64-based).
        let k_int = Key::single(Value::Int64((1i64 << 53) + 1)).unwrap();
        let k_float = Key::single(Value::Float64((1u64 << 53) as f64)).unwrap();
        assert_eq!(k_int, k_float);
        assert_eq!(hash_of(&k_int), hash_of(&k_float));
    }

    #[test]
    fn ipv4_and_mapped_ipv6_share_hash() {
        // An IPv4 and its IPv4-mapped IPv6 compare equal, so they must hash equal.
        let v4 = std::net::Ipv4Addr::new(1, 2, 3, 4);
        let k4 = Key::single(Value::Ipv4(v4)).unwrap();
        let k6 = Key::single(Value::Ipv6(v4.to_ipv6_mapped())).unwrap();
        assert_eq!(k4, k6);
        assert_eq!(hash_of(&k4), hash_of(&k6));
    }

    #[test]
    fn rejects_unsupported_types() {
        assert!(Key::single(Value::Null).is_err());
        assert!(Key::single(Value::Json(serde_json::Value::Null)).is_err());
        assert!(Key::single(Value::Object(vec![])).is_err());
    }

    #[test]
    fn composite_key_two_elements() {
        let k = Key::composite(vec![Value::Int64(1), Value::Text("a".into())]).unwrap();
        assert_eq!(k.len(), 2);
        assert!(k.into_single().is_none());
    }

    #[test]
    fn composite_key_ordering_is_lexicographic() {
        let k1 = Key::composite(vec![Value::Int64(1), Value::Text("b".into())]).unwrap();
        let k2 = Key::composite(vec![Value::Int64(1), Value::Text("a".into())]).unwrap();
        assert!(k1 > k2);
    }

    #[test]
    fn composite_key_shorter_is_less() {
        let k1 = Key::single(Value::Int64(1)).unwrap();
        let k2 = Key::composite(vec![Value::Int64(1), Value::Int64(2)]).unwrap();
        assert!(k1 < k2);
    }

    #[test]
    fn single_key_into_single() {
        let k = Key::single(Value::Text("hello".into())).unwrap();
        let v = k.into_single().unwrap();
        assert_eq!(v, Value::Text("hello".into()));
    }

    #[test]
    fn from_value_returns_none_for_null() {
        assert!(Key::from_value(&Value::Null).is_none());
    }

    #[test]
    fn decimal_normalized_equality() {
        let d1 = Key::single(Value::Decimal(BigDecimal::from_str("1.00").unwrap())).unwrap();
        let d2 = Key::single(Value::Decimal(BigDecimal::from_str("1.0").unwrap())).unwrap();
        // BigDecimal's own PartialEq is scale-aware, so 1.00 != 1.0 in Value terms.
        // But Key hashes by normalised string form, so they share a hash bucket.
        // Eq checks via BigDecimal::eq, which may differ — this is intentional:
        // the Key user should canonicalise decimals before insertion.
        assert_eq!(hash_of(&d1), hash_of(&d2));
    }

    #[test]
    fn ord_total_across_types() {
        let k_bool = Key::single(Value::Bool(true)).unwrap();
        let k_int = Key::single(Value::Int64(1)).unwrap();
        let k_text = Key::single(Value::Text("a".into())).unwrap();
        // Cross-type ordering falls back to discriminant — should not panic.
        let _ = k_bool.cmp(&k_int);
        let _ = k_int.cmp(&k_text);
    }

    #[test]
    fn empty_composite_rejected() {
        assert!(Key::composite(vec![]).is_err());
    }

    #[test]
    fn nan_sorts_after_normal_floats() {
        let k_nan = Key::single(Value::Float64(f64::NAN)).unwrap();
        let k_one = Key::single(Value::Float64(1.0)).unwrap();
        assert!(k_nan > k_one);
    }

    mod proptests {
        use super::*;
        use chrono::{NaiveDate, TimeZone, Utc};
        use proptest::prelude::*;
        use std::net::{Ipv4Addr, Ipv6Addr};
        use uuid::Uuid;

        // ─── Strategies ───────────────────────────────────────────────

        fn arb_key_value() -> impl Strategy<Value = Value> {
            prop_oneof![
                any::<bool>().prop_map(Value::Bool),
                any::<i64>().prop_map(Value::Int64),
                any::<i8>().prop_map(Value::Int8),
                any::<i16>().prop_map(Value::Int16),
                any::<i32>().prop_map(Value::Int32),
                any::<u8>().prop_map(Value::UInt8),
                any::<u16>().prop_map(Value::UInt16),
                any::<u32>().prop_map(Value::UInt32),
                any::<u64>().prop_map(Value::UInt64),
                any::<f64>().prop_map(Value::Float64),
                any::<f32>().prop_map(Value::Float32),
                ".*".prop_map(Value::Text),
                proptest::collection::vec(any::<u8>(), 0..32).prop_map(Value::Bytes),
                arb_bigint().prop_map(Value::BigInt),
                arb_ipv4().prop_map(Value::Ipv4),
                arb_ipv6().prop_map(Value::Ipv6),
                arb_uuid().prop_map(Value::Uuid),
                arb_date().prop_map(Value::Date),
                arb_timestamp().prop_map(Value::Timestamp),
            ]
        }

        fn arb_bigint() -> impl Strategy<Value = BigInt> {
            prop_oneof![
                // Small values that fit i64 — should hash-agree with Int64
                any::<i64>().prop_map(BigInt::from),
                // Large values exceeding i64
                any::<u64>()
                    .prop_filter("must exceed i64", |n| *n > i64::MAX as u64)
                    .prop_map(BigInt::from),
                // Negative large values
                any::<i64>().prop_map(|n| BigInt::from(n) * BigInt::from(i64::MAX)),
            ]
        }

        fn arb_ipv4() -> impl Strategy<Value = Ipv4Addr> {
            any::<[u8; 4]>().prop_map(Ipv4Addr::from)
        }

        fn arb_ipv6() -> impl Strategy<Value = Ipv6Addr> {
            any::<[u8; 16]>().prop_map(Ipv6Addr::from)
        }

        fn arb_uuid() -> impl Strategy<Value = Uuid> {
            any::<[u8; 16]>().prop_map(Uuid::from_bytes)
        }

        fn arb_date() -> impl Strategy<Value = NaiveDate> {
            // Valid date range for chrono
            (1970i32..2100i32, 1u32..13u32, 1u32..29u32).prop_map(|(y, m, d)| {
                NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap_or_else(|| NaiveDate::from_ymd_opt(y, m, 1).unwrap())
            })
        }

        fn arb_timestamp() -> impl Strategy<Value = chrono::DateTime<Utc>> {
            // Seconds since epoch in a reasonable range
            (0i64..4_000_000_000i64).prop_map(|secs| Utc.timestamp_opt(secs, 0).unwrap())
        }

        // ─── 1. Hash/Eq consistency across canonicalized types ────────

        #[test_strategy::proptest]
        fn hash_eq_int8_vs_int64(n: i8) {
            let k8 = Key::single(Value::Int8(n)).unwrap();
            let k64 = Key::single(Value::Int64(i64::from(n))).unwrap();
            assert_eq!(k8, k64, "Int8({n}) != Int64({n})");
            assert_eq!(
                hash_of(&k8),
                hash_of(&k64),
                "Int8({n}) hash != Int64({n}) hash"
            );
        }

        #[test_strategy::proptest]
        fn hash_eq_uint16_vs_int64(n: u16) {
            let ku16 = Key::single(Value::UInt16(n)).unwrap();
            let k64 = Key::single(Value::Int64(i64::from(n))).unwrap();
            assert_eq!(ku16, k64);
            assert_eq!(hash_of(&ku16), hash_of(&k64));
        }

        #[test_strategy::proptest]
        fn hash_eq_uint32_vs_int64(n: u32) {
            let ku32 = Key::single(Value::UInt32(n)).unwrap();
            let k64 = Key::single(Value::Int64(i64::from(n))).unwrap();
            assert_eq!(ku32, k64);
            assert_eq!(hash_of(&ku32), hash_of(&k64));
        }

        #[test_strategy::proptest]
        fn hash_eq_uint64_vs_int64_when_fits(#[strategy(0u64..=(i64::MAX as u64))] n: u64) {
            let ku64 = Key::single(Value::UInt64(n)).unwrap();
            let k64 = Key::single(Value::Int64(n as i64)).unwrap();
            assert_eq!(ku64, k64);
            assert_eq!(hash_of(&ku64), hash_of(&k64));
        }

        #[test_strategy::proptest]
        fn hash_eq_bigint_small_vs_int64(n: i64) {
            let k_big = Key::single(Value::BigInt(BigInt::from(n))).unwrap();
            let k64 = Key::single(Value::Int64(n)).unwrap();
            assert_eq!(k_big, k64);
            assert_eq!(
                hash_of(&k_big),
                hash_of(&k64),
                "BigInt({n}) hash != Int64({n}) hash"
            );
        }

        #[test_strategy::proptest]
        fn hash_eq_float32_vs_float64(#[strategy(proptest::num::f32::ANY)] f: f32) {
            if !f.is_finite() && !f.is_nan() {
                // +/- infinity
                let k32 = Key::single(Value::Float32(f)).unwrap();
                let k64 = Key::single(Value::Float64(f64::from(f))).unwrap();
                assert_eq!(k32, k64);
                assert_eq!(hash_of(&k32), hash_of(&k64));
            } else if f.is_finite() {
                let k32 = Key::single(Value::Float32(f)).unwrap();
                let k64 = Key::single(Value::Float64(f64::from(f))).unwrap();
                assert_eq!(k32, k64);
                assert_eq!(hash_of(&k32), hash_of(&k64));
            }
            // NaN case is covered separately
        }

        #[test_strategy::proptest]
        fn hash_eq_float32_nan_vs_float64_nan(
            // Use any f32 NaN bit pattern
            #[strategy(proptest::num::f32::ANY.prop_filter("nan", |f| f.is_nan()))] f: f32,
        ) {
            let k32 = Key::single(Value::Float32(f)).unwrap();
            let k64 = Key::single(Value::Float64(f64::NAN)).unwrap();
            assert_eq!(k32, k64, "Float32(NaN) must == Float64(NaN)");
            assert_eq!(
                hash_of(&k32),
                hash_of(&k64),
                "Float32(NaN) hash must == Float64(NaN) hash"
            );
        }

        #[test_strategy::proptest]
        fn hash_eq_float64_nan_all_bit_patterns(
            #[strategy(proptest::num::f64::ANY.prop_filter("nan", |f| f.is_nan()))] nan1: f64,
            #[strategy(proptest::num::f64::ANY.prop_filter("nan", |f| f.is_nan()))] nan2: f64,
        ) {
            let k1 = Key::single(Value::Float64(nan1)).unwrap();
            let k2 = Key::single(Value::Float64(nan2)).unwrap();
            assert_eq!(k1, k2, "all NaN patterns must be equal");
            assert_eq!(
                hash_of(&k1),
                hash_of(&k2),
                "all NaN patterns must hash same"
            );
        }

        #[test_strategy::proptest]
        fn hash_eq_float64_zero_variants(
            #[strategy(Just(0.0f64).prop_union(Just(-0.0f64)))] z1: f64,
            #[strategy(Just(0.0f64).prop_union(Just(-0.0f64)))] z2: f64,
        ) {
            let k1 = Key::single(Value::Float64(z1)).unwrap();
            let k2 = Key::single(Value::Float64(z2)).unwrap();
            assert_eq!(k1, k2, "0.0 and -0.0 must be equal");
            assert_eq!(hash_of(&k1), hash_of(&k2), "0.0 and -0.0 must hash same");
        }

        // ─── 2. Ord/Eq consistency ───────────────────────────────────

        #[test_strategy::proptest]
        fn ord_consistent_with_eq(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            let is_eq = ka == kb;
            let is_cmp_eq = ka.cmp(&kb) == Ordering::Equal;
            assert_eq!(
                is_eq,
                is_cmp_eq,
                "Eq and Ord disagree for {:?} vs {:?}",
                ka.values(),
                kb.values()
            );
        }

        // ─── 3. Ord antisymmetry ─────────────────────────────────────

        #[test_strategy::proptest]
        fn ord_antisymmetric(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            assert_eq!(
                ka.cmp(&kb),
                kb.cmp(&ka).reverse(),
                "antisymmetry violated for {:?} vs {:?}",
                ka.values(),
                kb.values()
            );
        }

        // ─── 4. Ord transitivity ─────────────────────────────────────

        #[test_strategy::proptest]
        fn ord_transitive(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
            #[strategy(arb_key_value())] c: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            let kc = Key::single(c).unwrap();
            let ab = ka.cmp(&kb);
            let bc = kb.cmp(&kc);
            let ac = ka.cmp(&kc);

            // If a < b and b < c then a < c
            if ab == Ordering::Less && bc == Ordering::Less {
                assert_eq!(
                    ac,
                    Ordering::Less,
                    "transitivity violated: {:?} < {:?} < {:?} but first.cmp(last) = {:?}",
                    ka.values(),
                    kb.values(),
                    kc.values(),
                    ac
                );
            }
            // If a > b and b > c then a > c
            if ab == Ordering::Greater && bc == Ordering::Greater {
                assert_eq!(
                    ac,
                    Ordering::Greater,
                    "transitivity violated (greater): {:?} > {:?} > {:?} but first.cmp(last) = {:?}",
                    ka.values(),
                    kb.values(),
                    kc.values(),
                    ac
                );
            }
            // If a == b and b == c then a == c
            if ab == Ordering::Equal && bc == Ordering::Equal {
                assert_eq!(
                    ac,
                    Ordering::Equal,
                    "transitivity violated (equal): {:?} == {:?} == {:?} but first.cmp(last) = {:?}",
                    ka.values(),
                    kb.values(),
                    kc.values(),
                    ac
                );
            }
        }

        // ─── 5. Composite key lexicographic ordering ─────────────────

        #[test_strategy::proptest]
        fn composite_lexicographic(
            #[strategy(any::<i64>())] prefix: i64,
            #[strategy(".*")] s1: String,
            #[strategy(".*")] s2: String,
        ) {
            let k1 = Key::composite(vec![Value::Int64(prefix), Value::Text(s1.clone())]).unwrap();
            let k2 = Key::composite(vec![Value::Int64(prefix), Value::Text(s2.clone())]).unwrap();
            // When prefix is equal, ordering is determined by second element
            assert_eq!(k1.cmp(&k2), s1.cmp(&s2));
        }

        #[test_strategy::proptest]
        fn composite_shorter_prefix_is_less(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let short = Key::single(a.clone()).unwrap();
            let long = Key::composite(vec![a, b]).unwrap();
            // If first elements are equal, shorter < longer
            if short.values()[0] == long.values()[0]
                || cmp_element(&short.values()[0], &long.values()[0]) == Ordering::Equal
            {
                assert!(
                    short < long,
                    "shorter prefix must be less: {:?} vs {:?}",
                    short.values(),
                    long.values()
                );
            }
        }

        // ─── 6. NaN total order ──────────────────────────────────────

        #[test_strategy::proptest]
        fn nan_greater_than_any_finite(
            #[strategy(proptest::num::f64::ANY.prop_filter("finite", |f| f.is_finite()))]
            finite: f64,
        ) {
            let k_nan = Key::single(Value::Float64(f64::NAN)).unwrap();
            let k_finite = Key::single(Value::Float64(finite)).unwrap();
            assert!(k_nan > k_finite, "NaN must sort after finite {finite}");
        }

        #[test_strategy::proptest]
        fn nan_greater_than_infinity() {
            let k_nan = Key::single(Value::Float64(f64::NAN)).unwrap();
            let k_inf = Key::single(Value::Float64(f64::INFINITY)).unwrap();
            let k_neg_inf = Key::single(Value::Float64(f64::NEG_INFINITY)).unwrap();
            assert!(k_nan > k_inf, "NaN must sort after +inf");
            assert!(k_nan > k_neg_inf, "NaN must sort after -inf");
        }

        // ─── 7. Construction never panics ────────────────────────────

        #[test_strategy::proptest]
        fn key_construction_never_panics(#[strategy(arb_key_value())] v: Value) {
            // Must not panic; may return Ok or Err
            let _ = Key::single(v);
        }

        #[test_strategy::proptest]
        fn composite_construction_never_panics(
            #[strategy(proptest::collection::vec(arb_key_value(), 1..5))] values: Vec<Value>,
        ) {
            let _ = Key::composite(values);
        }

        // ─── 8. UInt64 overflow promotes to BigInt correctly ─────────

        #[test_strategy::proptest]
        fn uint64_overflow_promotes_to_bigint(
            #[strategy((i64::MAX as u64 + 1)..=u64::MAX)] n: u64,
        ) {
            let k_u64 = Key::single(Value::UInt64(n)).unwrap();
            let k_big = Key::single(Value::BigInt(BigInt::from(n))).unwrap();
            assert_eq!(k_u64, k_big, "UInt64({n}) must equal BigInt({n})");
            assert_eq!(
                hash_of(&k_u64),
                hash_of(&k_big),
                "UInt64({n}) must hash same as BigInt({n})"
            );
        }

        // ─── 9. Decimal normalized hash agreement ────────────────────

        #[test_strategy::proptest]
        fn decimal_normalized_hash_agreement(
            #[strategy(-1_000_000i64..1_000_000i64)] mantissa: i64,
            #[strategy(0u32..5u32)] extra_zeros: u32,
        ) {
            use std::str::FromStr;
            // Build two representations of the same numeric value
            // e.g. "42.0" and "42.00"
            let base = format!("{mantissa}.0");
            let extended = format!("{mantissa}.{}", "0".repeat(1 + extra_zeros as usize));
            let d1 = BigDecimal::from_str(&base).unwrap();
            let d2 = BigDecimal::from_str(&extended).unwrap();
            let k1 = Key::single(Value::Decimal(d1)).unwrap();
            let k2 = Key::single(Value::Decimal(d2)).unwrap();
            assert_eq!(
                hash_of(&k1),
                hash_of(&k2),
                "Decimal({base}) and Decimal({extended}) must hash the same (normalized)"
            );
        }

        // ─── Broad eq_implies_hash across all types ──────────────────

        #[test_strategy::proptest]
        fn eq_implies_same_hash(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            if ka == kb {
                assert_eq!(
                    hash_of(&ka),
                    hash_of(&kb),
                    "equal keys must hash the same: {:?} vs {:?}",
                    ka.values(),
                    kb.values()
                );
            }
        }

        // ─── Cross-width Int hash agreement (all widths) ─────────────

        #[test_strategy::proptest]
        fn cross_int_width_hash_agreement(#[strategy(i8::MIN..=i8::MAX)] n: i8) {
            let k8 = Key::single(Value::Int8(n)).unwrap();
            let k64 = Key::single(Value::Int64(n as i64)).unwrap();
            assert_eq!(k8, k64);
            assert_eq!(hash_of(&k8), hash_of(&k64));
        }

        #[test_strategy::proptest]
        fn cross_int16_width_hash_agreement(n: i16) {
            let k16 = Key::single(Value::Int16(n)).unwrap();
            let k64 = Key::single(Value::Int64(i64::from(n))).unwrap();
            assert_eq!(k16, k64);
            assert_eq!(hash_of(&k16), hash_of(&k64));
        }

        #[test_strategy::proptest]
        fn cross_int32_width_hash_agreement(n: i32) {
            let k32 = Key::single(Value::Int32(n)).unwrap();
            let k64 = Key::single(Value::Int64(i64::from(n))).unwrap();
            assert_eq!(k32, k64);
            assert_eq!(hash_of(&k32), hash_of(&k64));
        }

        #[test_strategy::proptest]
        fn cross_uint8_width_hash_agreement(n: u8) {
            let ku8 = Key::single(Value::UInt8(n)).unwrap();
            let k64 = Key::single(Value::Int64(i64::from(n))).unwrap();
            assert_eq!(ku8, k64);
            assert_eq!(hash_of(&ku8), hash_of(&k64));
        }

        // ─── Composite hash/eq consistency ───────────────────────────

        #[test_strategy::proptest]
        fn composite_eq_implies_hash(
            #[strategy(proptest::collection::vec(arb_key_value(), 1..4))] vals_a: Vec<Value>,
            #[strategy(proptest::collection::vec(arb_key_value(), 1..4))] vals_b: Vec<Value>,
        ) {
            let ka = Key::composite(vals_a).unwrap();
            let kb = Key::composite(vals_b).unwrap();
            if ka == kb {
                assert_eq!(
                    hash_of(&ka),
                    hash_of(&kb),
                    "equal composite keys must hash the same"
                );
            }
        }

        // ─── Reflexivity ─────────────────────────────────────────────

        #[test_strategy::proptest]
        fn key_reflexive_eq(#[strategy(arb_key_value())] v: Value) {
            let k = Key::single(v).unwrap();
            assert_eq!(k, k.clone());
            assert_eq!(hash_of(&k), hash_of(&k.clone()));
            assert_eq!(k.cmp(&k.clone()), Ordering::Equal);
        }
    }
}
