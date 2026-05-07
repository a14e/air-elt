//! Hashable, totally-equal newtype around `bson::Bson` for use as a
//! `HashMap`/`HashSet` key. Bson itself implements `PartialEq` but not
//! `Eq`/`Hash` because of `Double(f64)` (NaN). `KeyBson` adds two
//! deviations from IEEE-754:
//!
//! * `NaN == NaN` (and all NaN bit patterns hash identically — we
//!   canonicalise to a single quiet-NaN bit pattern). NaN is not
//!   expected in `_id` keys, but locking the contract avoids future
//!   surprises.
//! * `Null == Null`, `Undefined == Undefined`, `MaxKey == MaxKey`,
//!   `MinKey == MinKey` — these are already total under Bson's own
//!   `PartialEq`, but they are surfaced explicitly here.
//!
//! Equality recurses through `Document`, `Array`, and
//! `JavaScriptCodeWithScope.scope` so a nested `Double` is handled by
//! the same NaN rule.

use std::hash::{Hash, Hasher};

use bson::{Bson, Document};

/// Canonical quiet-NaN bit pattern. Every NaN payload collapses to this
/// when hashing so distinct in-memory NaNs still bucket together.
const F64_CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

#[derive(Debug, Clone)]
pub struct KeyBson(pub Bson);

impl PartialEq for KeyBson {
    fn eq(&self, other: &Self) -> bool {
        bson_eq(&self.0, &other.0)
    }
}
impl Eq for KeyBson {}

impl Hash for KeyBson {
    fn hash<H: Hasher>(&self, state: &mut H) {
        bson_hash(&self.0, state);
    }
}

fn bson_eq(a: &Bson, b: &Bson) -> bool {
    match (a, b) {
        (Bson::Double(x), Bson::Double(y)) => {
            if x.is_nan() && y.is_nan() {
                true
            } else {
                x.to_bits() == y.to_bits()
            }
        }
        (Bson::Array(x), Bson::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| bson_eq(a, b))
        }
        (Bson::Document(x), Bson::Document(y)) => doc_eq(x, y),
        (Bson::JavaScriptCodeWithScope(x), Bson::JavaScriptCodeWithScope(y)) => {
            x.code == y.code && doc_eq(&x.scope, &y.scope)
        }
        // Variants that don't carry nested Bson — Bson's own PartialEq
        // is correct (no float / NaN payload at this point).
        _ => a == b,
    }
}

fn doc_eq(a: &Document, b: &Document) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b.iter())
            .all(|((ka, va), (kb, vb))| ka == kb && bson_eq(va, vb))
}

fn bson_hash<H: Hasher>(b: &Bson, h: &mut H) {
    // Mix the variant tag in first so payloads that happen to share
    // bytes across variants (e.g. `Int32(0)` vs `Int64(0)` vs `Null`)
    // land in different hash buckets. Eq already separates them; this
    // just keeps the hash from over-colliding.
    std::mem::discriminant(b).hash(h);
    match b {
        Bson::Double(f) => {
            let bits = if f.is_nan() {
                F64_CANONICAL_NAN
            } else {
                f.to_bits()
            };
            bits.hash(h);
        }
        Bson::String(s) | Bson::JavaScriptCode(s) | Bson::Symbol(s) => s.hash(h),
        Bson::Array(a) => {
            a.len().hash(h);
            for item in a {
                bson_hash(item, h);
            }
        }
        Bson::Document(d) => doc_hash(d, h),
        Bson::Boolean(v) => v.hash(h),
        Bson::Null | Bson::Undefined | Bson::MaxKey | Bson::MinKey => {}
        Bson::RegularExpression(r) => {
            r.pattern.hash(h);
            r.options.hash(h);
        }
        Bson::JavaScriptCodeWithScope(jcs) => {
            jcs.code.hash(h);
            doc_hash(&jcs.scope, h);
        }
        Bson::Int32(n) => n.hash(h),
        Bson::Int64(n) => n.hash(h),
        Bson::Timestamp(t) => {
            t.time.hash(h);
            t.increment.hash(h);
        }
        Bson::Binary(bin) => {
            u8::from(bin.subtype).hash(h);
            bin.bytes.hash(h);
        }
        Bson::ObjectId(id) => id.bytes().hash(h),
        Bson::DateTime(dt) => dt.timestamp_millis().hash(h),
        Bson::Decimal128(d) => d.bytes().hash(h),
        // DbPointer fields are not all stably accessible across bson
        // releases; discriminant-only hashing is sound (collisions are
        // allowed; the Eq impl still distinguishes payloads).
        Bson::DbPointer(_) => {}
    }
}

fn doc_hash<H: Hasher>(d: &Document, h: &mut H) {
    d.len().hash(h);
    for (k, v) in d {
        k.hash(h);
        bson_hash(v, h);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashSet;
    use std::hash::DefaultHasher;

    use bson::oid::ObjectId;
    use bson::{Bson, doc};

    use super::*;

    fn h(k: &KeyBson) -> u64 {
        let mut s = DefaultHasher::new();
        k.hash(&mut s);
        s.finish()
    }

    #[test]
    fn null_equals_null() {
        assert_eq!(KeyBson(Bson::Null), KeyBson(Bson::Null));
        assert_eq!(h(&KeyBson(Bson::Null)), h(&KeyBson(Bson::Null)));
    }

    #[test]
    fn undefined_equals_undefined() {
        assert_eq!(KeyBson(Bson::Undefined), KeyBson(Bson::Undefined));
        assert_eq!(h(&KeyBson(Bson::Undefined)), h(&KeyBson(Bson::Undefined)));
    }

    #[test]
    fn nan_equals_nan_and_hashes_same() {
        let a = KeyBson(Bson::Double(f64::NAN));
        let b = KeyBson(Bson::Double(f64::from_bits(0x7FF8_DEAD_BEEF_0042)));
        assert!(matches!(a.0, Bson::Double(x) if x.is_nan()));
        assert_eq!(a, b);
        assert_eq!(h(&a), h(&b));
    }

    #[test]
    fn distinct_int_widths_not_equal() {
        // Mongo `_id` types are stable per document; we treat Int32(1)
        // and Int64(1) as distinct (matching Bson's own PartialEq).
        assert_ne!(KeyBson(Bson::Int32(1)), KeyBson(Bson::Int64(1)));
    }

    #[test]
    fn objectid_equal_when_bytes_match() {
        let id = ObjectId::new();
        assert_eq!(KeyBson(Bson::ObjectId(id)), KeyBson(Bson::ObjectId(id)));
        assert_eq!(
            h(&KeyBson(Bson::ObjectId(id))),
            h(&KeyBson(Bson::ObjectId(id)))
        );
    }

    #[test]
    fn document_eq_recurses_through_nan() {
        let a = KeyBson(Bson::Document(doc! { "x": f64::NAN }));
        let b = KeyBson(Bson::Document(doc! { "x": f64::NAN }));
        assert_eq!(a, b);
        assert_eq!(h(&a), h(&b));
    }

    #[test]
    fn array_eq_recurses() {
        let a = KeyBson(Bson::Array(vec![Bson::Int64(1), Bson::Double(f64::NAN)]));
        let b = KeyBson(Bson::Array(vec![Bson::Int64(1), Bson::Double(f64::NAN)]));
        assert_eq!(a, b);
        assert_eq!(h(&a), h(&b));
    }

    #[test]
    fn distinct_variants_not_equal() {
        assert_ne!(KeyBson(Bson::Null), KeyBson(Bson::Int32(0)));
        assert_ne!(KeyBson(Bson::String("1".into())), KeyBson(Bson::Int32(1)));
    }

    #[test]
    fn binary_eq_distinguishes_subtype_and_bytes() {
        use bson::Binary;
        use bson::spec::BinarySubtype;
        let a = KeyBson(Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![1, 2, 3],
        }));
        let b = KeyBson(Bson::Binary(Binary {
            subtype: BinarySubtype::Generic,
            bytes: vec![1, 2, 3],
        }));
        let c = KeyBson(Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            bytes: vec![1, 2, 3],
        }));
        assert_eq!(a, b);
        assert_eq!(h(&a), h(&b));
        assert_ne!(a, c);
    }

    #[test]
    fn datetime_and_timestamp_eq() {
        use bson::{DateTime, Timestamp};
        let dt = DateTime::from_millis(1_700_000_000_000);
        assert_eq!(KeyBson(Bson::DateTime(dt)), KeyBson(Bson::DateTime(dt)));
        let ts = Timestamp {
            time: 42,
            increment: 7,
        };
        assert_eq!(KeyBson(Bson::Timestamp(ts)), KeyBson(Bson::Timestamp(ts)));
        assert_ne!(
            KeyBson(Bson::Timestamp(ts)),
            KeyBson(Bson::Timestamp(Timestamp {
                time: 42,
                increment: 8,
            }))
        );
    }

    #[test]
    fn decimal128_eq() {
        use bson::Decimal128;
        use std::str::FromStr;
        let d = Decimal128::from_str("1.23").unwrap();
        assert_eq!(KeyBson(Bson::Decimal128(d)), KeyBson(Bson::Decimal128(d)));
        assert_eq!(
            h(&KeyBson(Bson::Decimal128(d))),
            h(&KeyBson(Bson::Decimal128(d)))
        );
    }

    #[test]
    fn javascript_code_with_scope_recurses() {
        use bson::JavaScriptCodeWithScope;
        let a = KeyBson(Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
            code: "fn".into(),
            scope: doc! { "x": f64::NAN },
        }));
        let b = KeyBson(Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
            code: "fn".into(),
            scope: doc! { "x": f64::NAN },
        }));
        assert_eq!(a, b);
        assert_eq!(h(&a), h(&b));
    }

    #[test]
    fn hashset_buckets_correctly() {
        let mut set: HashSet<KeyBson> = HashSet::new();
        assert!(set.insert(KeyBson(Bson::Int64(7))));
        assert!(!set.insert(KeyBson(Bson::Int64(7))));
        assert!(set.insert(KeyBson(Bson::Double(f64::NAN))));
        assert!(!set.insert(KeyBson(Bson::Double(f64::NAN))));
        assert_eq!(set.len(), 2);
    }
}
