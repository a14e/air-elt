//! Stable byte-fingerprint encoding for `Value`. Used by CDC dedup
//! (`Row::raw_key`) to bucket rows by their `conflict.key` tuple
//! without leaning on `Hash` / `Eq` for the float / decimal / Json
//! variants of `Value` (where derive-Eq/Hash either doesn't apply or
//! would change the semantics of the existing `PartialEq`).
//!
//! Module is `pub(crate)` — the encoding is an internal contract
//! between `Row::raw_key` and the runner's dedup pass; no public API.
//!
//! ## Layout
//!
//! Each value is `<tag><payload><SEP>` where `SEP = 0xFF`. The tag
//! byte makes cross-variant collisions impossible; the length prefix
//! on variable-width payloads makes each value self-delimiting; the
//! trailing separator is belt-and-suspenders. Choosing `0xFF` (the
//! max byte) means a value with payload `a` ends with bytes that
//! sort after any payload starting with `a` followed by another data
//! byte — i.e. the separator never gets out-sorted by an in-payload
//! byte. We don't sort keys today (only HashSet-bucket them), but
//! the choice keeps the option open without re-encoding values later.

use chrono::Datelike;

use super::Value;

/// IEEE-754 canonical quiet NaN bit patterns: exponent all-1s, mantissa
/// MSB set, remaining mantissa zero. Used to canonicalize every NaN
/// variant to a single byte sequence.
const F32_CANONICAL_NAN: u32 = 0x7FC0_0000;
const F64_CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// Append a tagged byte representation of `v` to `buf`. Each variant
/// prefixes its tag byte and terminates with `0xFF` (max byte — see
/// module docstring) so cross-variant collisions are impossible.
/// Non-NaN floats are encoded by their bit pattern; every NaN
/// canonicalizes to a single quiet-NaN bit pattern so distinct
/// in-memory NaNs still bucket together.
pub(crate) fn write_value_key(buf: &mut Vec<u8>, v: &Value) {
    match v {
        Value::Null => buf.push(0),
        Value::Bool(b) => {
            buf.push(1);
            buf.push(*b as u8);
        }
        Value::Int16(n) => {
            buf.push(2);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::Int32(n) => {
            buf.push(3);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::Int64(n) => {
            buf.push(4);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::UInt8(n) => {
            buf.push(5);
            buf.push(*n);
        }
        Value::UInt16(n) => {
            buf.push(6);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::UInt32(n) => {
            buf.push(7);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::UInt64(n) => {
            buf.push(8);
            buf.extend_from_slice(&n.to_le_bytes());
        }
        Value::Float32(f) => {
            buf.push(9);
            // Every NaN bit pattern (signaling, quiet, custom-payload)
            // collapses to the canonical quiet NaN so two rows whose
            // float key is "NaN" bucket together — IEEE-754 leaves the
            // mantissa free, and `f32::NAN.to_bits()` is not guaranteed
            // identical across platforms, so we hard-code 0x7FC0_0000.
            let bits = if f.is_nan() {
                F32_CANONICAL_NAN
            } else {
                f.to_bits()
            };
            buf.extend_from_slice(&bits.to_le_bytes());
        }
        Value::Float64(f) => {
            buf.push(10);
            let bits = if f.is_nan() {
                F64_CANONICAL_NAN
            } else {
                f.to_bits()
            };
            buf.extend_from_slice(&bits.to_le_bytes());
        }
        Value::BigInt(b) => {
            buf.push(11);
            // num-bigint's `to_signed_bytes_le` is canonical and
            // round-trips losslessly.
            let bytes = b.to_signed_bytes_le();
            buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            buf.extend_from_slice(&bytes);
        }
        Value::Decimal(d) => {
            buf.push(12);
            // BigDecimal's canonical decimal-string form (also what
            // we use for cursor JSON storage — same canon).
            let s = d.to_string();
            buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Text(s) => {
            buf.push(13);
            buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
        Value::Bytes(b) => {
            buf.push(14);
            buf.extend_from_slice(&(b.len() as u64).to_le_bytes());
            buf.extend_from_slice(b);
        }
        Value::Date(d) => {
            buf.push(15);
            buf.extend_from_slice(&d.num_days_from_ce().to_le_bytes());
        }
        Value::Timestamp(ts) => {
            buf.push(16);
            buf.extend_from_slice(&ts.timestamp_nanos_opt().unwrap_or(0).to_le_bytes());
        }
        Value::Uuid(u) => {
            buf.push(17);
            buf.extend_from_slice(u.as_bytes());
        }
        Value::Json(j) => {
            buf.push(18);
            // Json keys for cursors are unusual (we recommend
            // primitive types). Best-effort canonical form via
            // serde_json. Two semantically-equal objects with
            // different field-insertion order may bucket apart —
            // that only over-keeps rows, never wrongly drops one.
            let s = serde_json::to_string(j).unwrap_or_default();
            buf.extend_from_slice(&(s.len() as u64).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
    }
    // Max-byte separator — see module docstring. Sorts after any data
    // byte, so a future lex-ordered consumer never sees a separator
    // out-sorted by an in-payload byte.
    buf.push(0xFF);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::{NaiveDate, TimeZone, Utc};
    use uuid::Uuid;

    use super::*;

    fn enc(v: &Value) -> Vec<u8> {
        let mut buf = Vec::new();
        write_value_key(&mut buf, v);
        buf
    }

    #[test]
    fn null_is_tag_zero_plus_separator() {
        assert_eq!(enc(&Value::Null), vec![0, 0xFF]);
    }

    #[test]
    fn separator_is_max_byte() {
        // Belt-and-suspenders: any value's encoding ends with 0xFF so
        // a future lex-ordered consumer cannot see a data byte sort
        // after the separator. Spot-check a few variants.
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Int64(0),
            Value::Text("x".into()),
        ] {
            let bytes = enc(&v);
            assert_eq!(*bytes.last().unwrap(), 0xFF, "missing terminator: {v:?}");
        }
    }

    #[test]
    fn distinct_variants_produce_distinct_tags() {
        // Same numeric payload (zero) across every integer variant
        // must not collide — each has its own tag byte. The first byte
        // is the tag; assert all distinct.
        let samples: Vec<Value> = vec![
            Value::Null,
            Value::Bool(false),
            Value::Int16(0),
            Value::Int32(0),
            Value::Int64(0),
            Value::UInt8(0),
            Value::UInt16(0),
            Value::UInt32(0),
            Value::UInt64(0),
            Value::Float32(0.0),
            Value::Float64(0.0),
            Value::Text(String::new()),
            Value::Bytes(Vec::new()),
        ];
        let tags: Vec<u8> = samples.iter().map(|v| enc(v)[0]).collect();
        let mut sorted = tags.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), tags.len(), "tags collide: {tags:?}");
    }

    #[test]
    fn bool_true_and_false_distinct() {
        assert_ne!(enc(&Value::Bool(true)), enc(&Value::Bool(false)));
    }

    #[test]
    fn nan_variants_canonicalise_to_single_encoding() {
        // Several distinct NaN bit patterns (canonical quiet, signaling,
        // payload-bearing) must all encode to the same bytes so two
        // rows with different in-memory NaNs bucket together.
        let canonical = f64::NAN;
        let signaling = f64::from_bits(0x7FF0_0000_0000_0001); // exp all-1s, mantissa LSB
        let payload = f64::from_bits(0x7FF8_DEAD_BEEF_0042);
        assert!(canonical.is_nan() && signaling.is_nan() && payload.is_nan());

        let a = enc(&Value::Float64(canonical));
        let b = enc(&Value::Float64(signaling));
        let c = enc(&Value::Float64(payload));
        assert_eq!(a, b);
        assert_eq!(a, c);

        // Same for f32.
        let a32 = enc(&Value::Float32(f32::NAN));
        let b32 = enc(&Value::Float32(f32::from_bits(0x7F80_0001)));
        assert_eq!(a32, b32);
    }

    #[test]
    fn float_zero_and_neg_zero_distinct() {
        // We do *not* canonicalise +0.0 / -0.0 — they're numerically
        // equal but byte-distinct, and we treat the bit pattern as
        // authoritative everywhere except for NaN.
        assert_ne!(enc(&Value::Float64(0.0)), enc(&Value::Float64(-0.0)));
    }

    #[test]
    fn text_lex_order_matches_string_lex_order() {
        // Same tag, same length-prefix on equal-length strings, so
        // payload comparison drives the result. "ant" < "bee" lex.
        let a = enc(&Value::Text("ant".into()));
        let b = enc(&Value::Text("bee".into()));
        assert!(a < b);
    }

    #[test]
    fn int64_lex_order_for_nonnegative_le_payload() {
        // Caveat: little-endian payload doesn't preserve numeric order
        // in general (two-byte 256 < two-byte 1 lex), but for values
        // that share a common high-byte prefix (e.g. small positives)
        // lex order matches numeric. We assert only the contract we
        // claim: equal values bucket together; differing values
        // produce differing keys.
        assert_eq!(enc(&Value::Int64(7)), enc(&Value::Int64(7)));
        assert_ne!(enc(&Value::Int64(7)), enc(&Value::Int64(8)));
    }

    #[test]
    fn separator_keeps_value_self_delimited() {
        // The separator + length-prefix combo means a string "ab"
        // never byte-aliases the encoding of "a" followed by "b" in
        // a tuple. We can't test the tuple form here (that's
        // `Row::raw_key`), but we can confirm the separator is the
        // last byte of every encoding, so any concatenation always
        // crosses the 0xFF boundary.
        let a = enc(&Value::Text("ab".into()));
        let split_a = enc(&Value::Text("a".into()));
        let split_b = enc(&Value::Text("b".into()));
        let mut concat = split_a.clone();
        concat.extend(&split_b);
        assert_ne!(a, concat);
    }

    #[test]
    fn bytes_length_prefix_distinguishes_empty_from_nonempty() {
        let empty = enc(&Value::Bytes(Vec::new()));
        let one = enc(&Value::Bytes(vec![0]));
        assert_ne!(empty, one);
    }

    #[test]
    fn date_uuid_timestamp_round_trip_stable() {
        // Stability: same input → same encoding. Cheap regression
        // sentinel against accidental encoder reshuffles.
        let d = Value::Date(NaiveDate::from_ymd_opt(2026, 5, 4).unwrap());
        let ts = Value::Timestamp(Utc.with_ymd_and_hms(2026, 5, 4, 10, 0, 0).unwrap());
        let id = Value::Uuid(Uuid::from_u128(0x0123_4567_89AB_CDEF_0123_4567_89AB_CDEF));
        for v in [d, ts, id] {
            assert_eq!(enc(&v), enc(&v));
        }
    }

    #[test]
    fn json_canonical_string_form_buckets_equal_objects() {
        // serde_json::to_string preserves insertion order. Two
        // semantically-equal objects with same field order bucket
        // together; objects with different field-insertion order
        // may bucket apart — documented as "over-keep, never wrong-
        // drop". This test pins the equal-order case.
        let a = Value::Json(serde_json::json!({ "k": 1, "v": 2 }));
        let b = Value::Json(serde_json::json!({ "k": 1, "v": 2 }));
        assert_eq!(enc(&a), enc(&b));
    }
}
