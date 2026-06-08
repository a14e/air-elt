//! Small shared primitives for value↔text conversion, kept in one place so the
//! conversion modules ([`to_text`](super::to_text), [`text_bool`](super::text_bool),
//! [`dispatch`](super::dispatch)) and the JSON encoder
//! ([`json_encode`](crate::json_encode)) share one definition:
//!
//! * [`bytes_to_hex`] — lowercase hex of a byte slice (binary renders
//!   identically in JSON and in `* → Text`);
//! * [`truncate_to_chars`] — in-place, codepoint-bounded truncation of an owned
//!   `String` (no realloc);
//! * [`parse_bool`] — the case-insensitive `text → bool` lexer.

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Lowercase hex encoding of a byte slice — two chars per byte, no separator.
pub(crate) fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Absorb the owned `String` and truncate it in place to at most `max_chars`
/// codepoints. Callers almost always own the string they just built, so
/// truncating in place via [`String::truncate`] avoids the realloc+copy a
/// slice-then-`to_string` would incur. `String::truncate` only resets the
/// length (no reallocation), and the byte index is taken at a codepoint
/// boundary so it never panics.
///
/// `Text { size: Some(N) }` counts characters, not bytes (matches
/// `information_schema.character_maximum_length` in PG and MySQL) — a 6-char /
/// 12-byte string fits a `varchar(10)` and must not be cropped on byte length.
pub(crate) fn truncate_to_chars(mut s: String, max_chars: usize) -> String {
    if max_chars == 0 {
        s.clear();
        return s;
    }
    if let Some((byte_idx, _)) = s.char_indices().nth(max_chars) {
        s.truncate(byte_idx);
    }
    s
}

const MAX_BOOL_TOKEN_BYTES: usize = 5;

/// Case-insensitive `text → bool` lexer. Truthy: `y / t / 1 / true / yes`.
/// Falsy: `n / f / 0 / false / no`. Anything else (empty, whitespace-padded,
/// `"2"`, `"maybe"`, non-ASCII) returns `None`.
///
/// Zero-alloc: every accepted token is ≤ 5 ASCII bytes, so the input is copied
/// into a fixed stack buffer (lowercased on the fly) and matched. Anything
/// longer than 5 bytes or non-ASCII is rejected before the buffer fill — no
/// valid token can match.
pub(crate) fn parse_bool(s: &str) -> Option<bool> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_BOOL_TOKEN_BYTES {
        return None;
    }
    let mut buf = [0u8; MAX_BOOL_TOKEN_BYTES];
    for (i, b) in bytes.iter().enumerate() {
        if !b.is_ascii() {
            return None;
        }
        buf[i] = b.to_ascii_lowercase();
    }
    match &buf[..bytes.len()] {
        b"y" | b"t" | b"1" | b"true" | b"yes" => Some(true),
        b"n" | b"f" | b"0" | b"false" | b"no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    // ---- bytes_to_hex -----------------------------------------------------

    #[test]
    fn bytes_to_hex_encodes_lowercase_no_separator() {
        assert_eq!(bytes_to_hex(&[0x01, 0xab, 0xff]), "01abff");
        assert_eq!(bytes_to_hex(&[]), "");
        assert_eq!(bytes_to_hex(&[0x00]), "00");
    }

    // ---- truncate_to_chars ------------------------------------------------

    #[test]
    fn truncate_spec_table() {
        let cases: &[(&str, usize, &str)] = &[
            ("", 5, ""),
            ("hi", 10, "hi"),
            ("0123456789ab", 10, "0123456789"),
            ("Привет", 10, "Привет"),
            ("Привет", 2, "Пр"),
            ("😀", 1, "😀"),
            ("a😀b", 2, "a😀"),
            ("abc", 0, ""),
            ("Привет", 0, ""),
            ("Привет", 6, "Привет"),
            ("abc", 3, "abc"),
        ];
        for (input, max_chars, expected) in cases {
            assert_eq!(
                truncate_to_chars(input.to_string(), *max_chars),
                *expected,
                "input={input:?}, max_chars={max_chars}",
            );
        }
    }

    fn arb_unicode_string() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..20)
            .prop_map(|cs| cs.into_iter().collect::<String>())
    }

    #[test_strategy::proptest]
    fn truncate_idempotent_when_within_limit(
        #[strategy(arb_unicode_string())] s: String,
        #[strategy(0usize..40)] extra: usize,
    ) {
        let max = s.chars().count() + extra;
        prop_assert_eq!(truncate_to_chars(s.clone(), max), s);
    }

    #[test_strategy::proptest]
    fn truncate_yields_exactly_n_chars_when_exceeding(
        #[strategy(arb_unicode_string())] s: String,
        #[strategy(0usize..20)] max: usize,
    ) {
        prop_assume!(s.chars().count() > max);
        let out = truncate_to_chars(s.clone(), max);
        prop_assert_eq!(out.chars().count(), max);
        prop_assert!(s.starts_with(&out));
    }

    // ---- parse_bool -------------------------------------------------------

    #[test]
    fn parse_bool_accepted_tokens() {
        let truthy: &[&str] = &[
            "y", "Y", "t", "T", "1", "true", "TRUE", "TrUe", "yes", "YES",
        ];
        for s in truthy {
            assert_eq!(parse_bool(s), Some(true), "{s:?}");
        }
        let falsy: &[&str] = &[
            "n", "N", "f", "F", "0", "false", "FALSE", "FaLsE", "no", "NO",
        ];
        for s in falsy {
            assert_eq!(parse_bool(s), Some(false), "{s:?}");
        }
    }

    #[test]
    fn parse_bool_rejected_tokens() {
        let rejected: &[&str] = &["", "maybe", "2", "truee", " yes ", "yes ", " y", "é", "дa"];
        for s in rejected {
            assert_eq!(parse_bool(s), None, "{s:?}");
        }
    }

    fn mixed_case_token(
        canonical: &'static str,
        expected: bool,
    ) -> impl Strategy<Value = (String, bool)> {
        let len = canonical.len();
        prop::collection::vec(any::<bool>(), len).prop_map(move |mask| {
            let mut out = String::with_capacity(len);
            for (ch, upper) in canonical.chars().zip(mask) {
                if upper {
                    out.extend(ch.to_uppercase());
                } else {
                    out.extend(ch.to_lowercase());
                }
            }
            (out, expected)
        })
    }

    fn any_case_mixed_bool_token() -> impl Strategy<Value = (String, bool)> {
        prop_oneof![
            mixed_case_token("true", true),
            mixed_case_token("yes", true),
            mixed_case_token("t", true),
            mixed_case_token("y", true),
            Just(("1".to_string(), true)),
            mixed_case_token("false", false),
            mixed_case_token("no", false),
            mixed_case_token("f", false),
            mixed_case_token("n", false),
            Just(("0".to_string(), false)),
        ]
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn parse_bool_case_insensitive(#[strategy(any_case_mixed_bool_token())] item: (String, bool)) {
        let (token, expected) = item;
        prop_assert_eq!(parse_bool(&token), Some(expected), "token = {:?}", token);
    }

    fn arb_ascii_ws() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(vec![' ', '\t', '\n', '\r']), 0..4)
            .prop_map(|cs| cs.into_iter().collect())
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn parse_bool_rejects_whitespace_padded(
        #[strategy(any_case_mixed_bool_token())] item: (String, bool),
        #[strategy(arb_ascii_ws())] left: String,
        #[strategy(arb_ascii_ws())] right: String,
    ) {
        prop_assume!(!(left.is_empty() && right.is_empty()));
        let padded = format!("{left}{}{right}", item.0);
        prop_assert_eq!(parse_bool(&padded), None, "padded = {:?}", padded);
    }

    fn arb_non_ascii_string() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 1..6)
            .prop_map(|cs| cs.into_iter().collect::<String>())
            .prop_filter("must contain a non-ASCII char", |s| !s.is_ascii())
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn parse_bool_rejects_non_ascii(#[strategy(arb_non_ascii_string())] s: String) {
        prop_assert_eq!(parse_bool(&s), None, "input = {:?}", s);
    }
}
