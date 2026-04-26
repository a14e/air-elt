//! UTF-safe byte-prefix.
//!
//! Returns a slice of `s` with at most `max_bytes` bytes, rounded *down* to
//! the last UTF-8 codepoint boundary. Never returns a partial codepoint.

pub fn truncate_utf8(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk char_indices and remember the last byte-index that fits.
    let mut last_ok = 0usize;
    for (idx, ch) in s.char_indices() {
        let end = idx + ch.len_utf8();
        if end > max_bytes {
            break;
        }
        last_ok = end;
    }
    &s[..last_ok]
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_input() {
        assert_eq!(truncate_utf8("", 5), "");
    }

    #[test]
    fn fits_returns_whole() {
        assert_eq!(truncate_utf8("hi", 10), "hi");
    }

    #[test]
    fn ascii_cut() {
        assert_eq!(truncate_utf8("0123456789ab", 10), "0123456789");
    }

    #[test]
    fn cyrillic_rounds_down() {
        // "Привет" = 12 bytes (6 chars × 2 bytes). max=5 → "Пр" (4 bytes).
        assert_eq!(truncate_utf8("Привет", 5), "Пр");
    }

    #[test]
    fn emoji_split_rounds_down_to_empty() {
        // "😀" is 4 bytes. max=3 → "" (can't fit any codepoint).
        assert_eq!(truncate_utf8("😀", 3), "");
    }

    #[test]
    fn emoji_exact_fits() {
        assert_eq!(truncate_utf8("😀", 4), "😀");
    }

    #[test]
    fn emoji_then_ascii() {
        // "a😀b" = 1+4+1 = 6 bytes. max=5 → "a😀" (5 bytes).
        assert_eq!(truncate_utf8("a😀b", 5), "a😀");
    }

    #[test]
    fn one_byte_short_of_two_byte_char() {
        // "é" = 2 bytes. max=1 → "" (no codepoint fits).
        assert_eq!(truncate_utf8("é", 1), "");
    }

    #[test]
    fn zero_max_yields_empty() {
        assert_eq!(truncate_utf8("abc", 0), "");
    }
}
