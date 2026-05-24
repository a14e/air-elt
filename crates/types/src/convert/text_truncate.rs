//! Character-bounded text prefix.
//!
//! `Text { size: Some(N) }` represents a column whose declared length `N`
//! is in **characters** (matches `information_schema.character_maximum_length`
//! semantics in both PG and MySQL). Truncation therefore counts codepoints,
//! not bytes — a 6-char multibyte string (6 chars / 12 bytes) fits a
//! `varchar(10)` and must not be cropped to 5 chars just because a
//! 10-byte limit would demand it.
//!
//! Returns a `&str` slice prefix with at most `max_chars` codepoints.

pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    if max_chars == 0 {
        return "";
    }
    match s.char_indices().nth(max_chars) {
        Some((byte_idx, _)) => &s[..byte_idx],
        None => s,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Spec table — fixed inputs that pin down concrete behaviour at
    /// representative codepoint widths.
    #[test]
    fn spec_table() {
        // (input, max_chars, expected)
        let cases: &[(&str, usize, &str)] = &[
            // Empty input.
            ("", 5, ""),
            // Fits within budget — returns whole string.
            ("hi", 10, "hi"),
            // ASCII narrow cut at exact codepoint boundary.
            ("0123456789ab", 10, "0123456789"),
            // Cyrillic: chars, not bytes — "Привет" is 6 chars / 12 bytes.
            ("Привет", 10, "Привет"),
            ("Привет", 2, "Пр"),
            // Emoji counted as one char (4 bytes in UTF-8).
            ("😀", 1, "😀"),
            ("a😀b", 2, "a😀"),
            // Zero budget always yields empty regardless of contents.
            ("abc", 0, ""),
            ("Привет", 0, ""),
            // Exact char-count fit — no truncation.
            ("Привет", 6, "Привет"),
            ("abc", 3, "abc"),
        ];
        for (input, max_chars, expected) in cases {
            assert_eq!(
                truncate_chars(input, *max_chars),
                *expected,
                "input={input:?}, max_chars={max_chars}",
            );
        }
    }

    // ---- Property-based tests --------------------------------------

    /// Random Unicode strings of varied codepoint counts. Drawn from the
    /// full `char` universe so the strategy exercises codepoints across
    /// all UTF-8 width classes (1, 2, 3, 4 bytes).
    fn arb_unicode_string() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..20)
            .prop_map(|cs| cs.into_iter().collect::<String>())
    }

    /// When the input already fits in `max_chars` codepoints, truncation
    /// is the identity.
    #[test_strategy::proptest]
    fn text_truncate_idempotent_when_within_limit(
        #[strategy(arb_unicode_string())] s: String,
        #[strategy(0usize..40)] extra: usize,
    ) {
        let char_count = s.chars().count();
        let max = char_count + extra;
        prop_assert_eq!(truncate_chars(&s, max), s.as_str());
    }

    /// When the input exceeds `max_chars`, the result has exactly
    /// `max_chars` codepoints (not bytes) and is a valid prefix.
    #[test_strategy::proptest]
    fn text_truncate_yields_exactly_n_chars_when_exceeding(
        #[strategy(arb_unicode_string())] s: String,
        #[strategy(0usize..20)] max: usize,
    ) {
        let char_count = s.chars().count();
        prop_assume!(char_count > max);
        let out = truncate_chars(&s, max);
        prop_assert_eq!(out.chars().count(), max);
        prop_assert!(s.starts_with(out));
        // truncate_chars produces only valid UTF-8 boundaries by
        // construction — Rust's &str invariants enforce it.
    }
}
