//! `Text → Bool` lexer.
//!
//! Case-insensitive, no whitespace trimming. Truthy: `y / t / 1 / true /
//! yes`. Falsy: `n / f / 0 / false / no`. Anything else (incl. empty,
//! whitespace-padded, "2", "maybe") returns [`ConvertError::InvalidBool`].
//!
//! Allocation-free: every accepted token is ≤ 5 ASCII bytes, so we copy
//! the input into a small stack buffer (lowercased on the fly) and match.
//! Anything longer than 5 bytes or non-ASCII is rejected outright — no
//! valid token can match.

use super::error::ConvertError;
use crate::{DataType, Value};

const MAX_TOKEN_BYTES: usize = 5;

pub fn convert(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    match parse(&s) {
        Some(b) => Ok(Value::Bool(b)),
        None => Err(ConvertError::InvalidBool { value: s }),
    }
}

/// Zero-alloc implementation: every accepted token is at most 5 ASCII
/// bytes, so we lowercase into a fixed `[u8; 5]` stack buffer and match
/// against byte-string literals. No `String` is created on any path,
/// including rejection. Inputs longer than 5 bytes or containing any
/// non-ASCII byte are rejected before the buffer fill.
fn parse(s: &str) -> Option<bool> {
    let bytes = s.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_TOKEN_BYTES {
        return None;
    }
    let mut buf = [0u8; MAX_TOKEN_BYTES];
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

    /// Spec table — accepted tokens (case variants) and rejected inputs,
    /// pinned by example. Properties below cover the broader contract.
    #[test]
    fn accepted_tokens() {
        let truthy: &[&str] = &[
            "y", "Y", "t", "T", "1", "true", "TRUE", "TrUe", "yes", "YES",
        ];
        for s in truthy {
            assert_eq!(parse(s), Some(true), "{s:?}");
        }
        let falsy: &[&str] = &[
            "n", "N", "f", "F", "0", "false", "FALSE", "FaLsE", "no", "NO",
        ];
        for s in falsy {
            assert_eq!(parse(s), Some(false), "{s:?}");
        }
    }

    #[test]
    fn rejected_tokens() {
        // Empty, unknown, whitespace-padded, oversize, non-ASCII — all
        // refused by the parser.
        let rejected: &[&str] = &["", "maybe", "2", "truee", " yes ", "yes ", " y", "é", "дa"];
        for s in rejected {
            assert_eq!(parse(s), None, "{s:?}");
        }
    }

    #[test]
    fn convert_value_shape_mismatch() {
        let res = convert(Value::Int32(1), &DataType::Text { size: None });
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn convert_returns_invalid_bool_for_unknown_token() {
        let res = convert(Value::Text("maybe".into()), &DataType::Text { size: None });
        assert!(matches!(res, Err(ConvertError::InvalidBool { .. })));
    }

    #[test]
    fn convert_returns_bool_for_truthy_text() {
        let out = convert(Value::Text("yes".into()), &DataType::Text { size: None }).unwrap();
        assert_eq!(out, Value::Bool(true));
    }

    // ---- Property-based tests --------------------------------------

    /// Re-cases each character of an ASCII token independently, mixing
    /// upper and lower case. Returns the recased token together with
    /// its expected boolean.
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
    fn text_bool_case_insensitive_tokens(
        #[strategy(any_case_mixed_bool_token())] item: (String, bool),
    ) {
        let (token, expected) = item;
        prop_assert_eq!(parse(&token), Some(expected), "token = {:?}", token);
    }

    /// Wrapping any accepted token with ASCII whitespace must be rejected
    /// — the parser deliberately does not trim. Confirms the "no leading
    /// or trailing space" half of the contract under random padding.
    fn arb_ascii_ws() -> impl Strategy<Value = String> {
        // Plain ASCII whitespace bytes: space, tab, newline, carriage
        // return. Keep the budget small (token ≤ 5 chars, padded total
        // stays under MAX_TOKEN_BYTES + a margin) so we exercise both
        // "rejected because >5 bytes" and "rejected because non-token"
        // code paths.
        prop::collection::vec(prop::sample::select(vec![' ', '\t', '\n', '\r']), 0..4)
            .prop_map(|cs| cs.into_iter().collect())
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn text_bool_rejects_whitespace_padded_tokens(
        #[strategy(any_case_mixed_bool_token())] item: (String, bool),
        #[strategy(arb_ascii_ws())] left: String,
        #[strategy(arb_ascii_ws())] right: String,
    ) {
        // Skip the unpadded case — that's the canonical-accept path.
        prop_assume!(!(left.is_empty() && right.is_empty()));
        let padded = format!("{left}{}{right}", item.0);
        prop_assert_eq!(parse(&padded), None, "padded = {:?}", padded);
    }

    /// A string that contains any non-ASCII byte is always rejected,
    /// regardless of what canonical token it might "wrap". Covers emoji,
    /// Cyrillic, Latin extended, etc.
    fn arb_non_ascii_string() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 1..6)
            .prop_map(|cs| cs.into_iter().collect::<String>())
            .prop_filter("must contain a non-ASCII char", |s| !s.is_ascii())
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn text_bool_rejects_non_ascii(#[strategy(arb_non_ascii_string())] s: String) {
        prop_assert_eq!(parse(&s), None, "input = {:?}", s);
        let res = convert(Value::Text(s.clone()), &DataType::Text { size: None });
        let is_invalid = matches!(res, Err(ConvertError::InvalidBool { .. }));
        prop_assert!(is_invalid);
    }
}
