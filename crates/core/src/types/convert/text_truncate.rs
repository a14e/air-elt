//! Character-bounded text prefix.
//!
//! `Text { size: Some(N) }` represents a column whose declared length `N`
//! is in **characters** (matches `information_schema.character_maximum_length`
//! semantics in both PG and MySQL). Truncation therefore counts codepoints,
//! not bytes — `"Привет"` (6 chars / 12 bytes) fits a `varchar(10)` and
//! must not be cropped to 5 chars just because a 10-byte limit would
//! demand it.
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

    #[test]
    fn empty_input() {
        assert_eq!(truncate_chars("", 5), "");
    }

    #[test]
    fn fits_returns_whole() {
        assert_eq!(truncate_chars("hi", 10), "hi");
    }

    #[test]
    fn ascii_cut() {
        assert_eq!(truncate_chars("0123456789ab", 10), "0123456789");
    }

    #[test]
    fn cyrillic_counted_in_chars_not_bytes() {
        // "Привет" = 6 chars / 12 bytes. max_chars=10 must accept all 6.
        assert_eq!(truncate_chars("Привет", 10), "Привет");
        // max_chars=2 → "Пр" (2 chars / 4 bytes).
        assert_eq!(truncate_chars("Привет", 2), "Пр");
    }

    #[test]
    fn emoji_counts_as_one_char() {
        // "😀" is one char (4 bytes). max_chars=1 must keep it.
        assert_eq!(truncate_chars("😀", 1), "😀");
    }

    #[test]
    fn emoji_then_ascii() {
        // "a😀b" = 3 chars. max_chars=2 → "a😀".
        assert_eq!(truncate_chars("a😀b", 2), "a😀");
    }

    #[test]
    fn zero_max_yields_empty() {
        assert_eq!(truncate_chars("abc", 0), "");
        assert_eq!(truncate_chars("Привет", 0), "");
    }

    #[test]
    fn exact_char_count() {
        // max equals the number of chars — no truncation.
        assert_eq!(truncate_chars("Привет", 6), "Привет");
        assert_eq!(truncate_chars("abc", 3), "abc");
    }
}
