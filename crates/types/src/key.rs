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
        Value::Int64(n) => {
            0u8.hash(state);
            n.hash(state);
        }
        Value::BigInt(b) => {
            0u8.hash(state);
            match b.to_i64() {
                Some(n) => n.hash(state),
                None => b.hash(state),
            }
        }
        Value::Bool(b) => {
            1u8.hash(state);
            b.hash(state);
        }
        Value::Float64(f) => {
            2u8.hash(state);
            normalise_float_bits(*f).hash(state);
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
        Value::Ipv4(a) => {
            9u8.hash(state);
            a.hash(state);
        }
        Value::Ipv6(a) => {
            10u8.hash(state);
            a.hash(state);
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
        use proptest::prelude::*;

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
            ]
        }

        #[test_strategy::proptest]
        fn key_construction_never_panics(#[strategy(arb_key_value())] v: Value) {
            let _ = Key::single(v);
        }

        #[test_strategy::proptest]
        fn eq_implies_same_hash(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            if ka == kb {
                assert_eq!(hash_of(&ka), hash_of(&kb));
            }
        }

        #[test_strategy::proptest]
        fn ord_consistent_with_eq(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            let is_eq = ka == kb;
            let is_cmp_eq = ka.cmp(&kb) == Ordering::Equal;
            assert_eq!(is_eq, is_cmp_eq);
        }

        #[test_strategy::proptest]
        fn ord_antisymmetric(
            #[strategy(arb_key_value())] a: Value,
            #[strategy(arb_key_value())] b: Value,
        ) {
            let ka = Key::single(a).unwrap();
            let kb = Key::single(b).unwrap();
            assert_eq!(ka.cmp(&kb), kb.cmp(&ka).reverse());
        }

        #[test_strategy::proptest]
        fn cross_int_width_hash_agreement(#[strategy(i8::MIN..=i8::MAX)] n: i8) {
            let k8 = Key::single(Value::Int8(n)).unwrap();
            let k64 = Key::single(Value::Int64(n as i64)).unwrap();
            assert_eq!(k8, k64);
            assert_eq!(hash_of(&k8), hash_of(&k64));
        }
    }
}
