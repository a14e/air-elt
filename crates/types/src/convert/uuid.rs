//! UUID parsing and rendering helpers shared across connectors.
//!
//! These are pure functions — they know nothing about `Value`. They accept
//! borrowed bytes/strings and return a typed `Uuid` or its serialised form.
//! The `convert` dispatcher in `super` wraps them around `Value` variants.
//!
//! Accepted text formats (case-insensitive):
//!
//! 1. Canonical hyphenated `8-4-4-4-12` (36 chars).
//! 2. Hex-only, no separators (32 chars).
//! 3. Microsoft-style with curly braces around the canonical form (38 chars).
//!
//! Rendering always produces the canonical lower-case form with hyphens.

use uuid::Uuid;

use super::error::ConvertError;

/// Parse a UUID from a textual representation. Trims surrounding whitespace.
///
/// Zero-allocation: walks the trimmed input byte-by-byte, skips dashes,
/// validates hex on the fly, and packs the 16 result bytes directly into a
/// stack array. No intermediate `String` is built on either the success or
/// the failure path. Pathologically long inputs are rejected after the
/// first 36 byte-budget overflow rather than scanned in full.
pub fn parse_text(input: &str) -> Result<Uuid, ConvertError> {
    let trimmed = input.trim().as_bytes();
    let stripped: &[u8] =
        if trimmed.first() == Some(&b'{') && trimmed.last() == Some(&b'}') && trimmed.len() >= 2 {
            &trimmed[1..trimmed.len() - 1]
        } else {
            trimmed
        };

    if stripped.len() > 36 {
        return Err(ConvertError::InvalidUuid {
            reason: format!("input too long: {} bytes", stripped.len()),
        });
    }

    let mut bytes = [0u8; 16];
    let mut hi: Option<u8> = None;
    let mut written = 0usize;
    for &b in stripped {
        if b == b'-' {
            continue;
        }
        let nibble = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            _ => return Err(ConvertError::InvalidHex),
        };
        match hi {
            None => hi = Some(nibble),
            Some(h) => {
                if written == 16 {
                    return Err(ConvertError::InvalidUuid {
                        reason: "more than 32 hex digits".into(),
                    });
                }
                bytes[written] = (h << 4) | nibble;
                written += 1;
                hi = None;
            }
        }
    }
    if hi.is_some() || written != 16 {
        return Err(ConvertError::InvalidUuid {
            reason: format!(
                "expected 32 hex digits, got {}",
                written * 2 + hi.map_or(0, |_| 1)
            ),
        });
    }
    Ok(Uuid::from_bytes(bytes))
}

/// Canonical lower-case form, 36 chars with hyphens. Production `Uuid → Text`
/// renders through [`value_to_string`](crate::value_to_string); this is a
/// test-only helper for the round-trip assertions below.
#[cfg(test)]
pub(crate) fn to_text(uuid: Uuid) -> String {
    uuid.hyphenated().to_string()
}

/// Build a UUID from exactly 16 raw bytes.
pub fn from_bytes(bytes: &[u8]) -> Result<Uuid, ConvertError> {
    if bytes.len() != 16 {
        return Err(ConvertError::Length {
            expected: 16,
            got: bytes.len(),
        });
    }
    let mut arr = [0u8; 16];
    arr.copy_from_slice(bytes);
    Ok(Uuid::from_bytes(arr))
}

/// 16-byte big-endian raw form.
pub fn to_bytes(uuid: Uuid) -> [u8; 16] {
    *uuid.as_bytes()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const SAMPLE: &str = "550e8400-e29b-41d4-a716-446655440000";
    const SAMPLE_NO_DASH: &str = "550e8400e29b41d4a716446655440000";
    const SAMPLE_BRACED: &str = "{550e8400-e29b-41d4-a716-446655440000}";
    const SAMPLE_UPPER: &str = "550E8400-E29B-41D4-A716-446655440000";

    #[test]
    fn parses_canonical() {
        let u = parse_text(SAMPLE).unwrap();
        assert_eq!(to_text(u), SAMPLE);
    }

    #[test]
    fn parses_hex_only() {
        let u = parse_text(SAMPLE_NO_DASH).unwrap();
        assert_eq!(to_text(u), SAMPLE);
    }

    #[test]
    fn parses_braced() {
        let u = parse_text(SAMPLE_BRACED).unwrap();
        assert_eq!(to_text(u), SAMPLE);
    }

    #[test]
    fn parses_upper_case() {
        let u = parse_text(SAMPLE_UPPER).unwrap();
        assert_eq!(to_text(u), SAMPLE);
    }

    #[test]
    fn rejects_too_long() {
        let huge = "a".repeat(1000);
        assert!(matches!(
            parse_text(&huge),
            Err(ConvertError::InvalidUuid { .. })
        ));
    }

    #[test]
    fn rejects_short() {
        assert!(matches!(
            parse_text("550e84"),
            Err(ConvertError::InvalidUuid { .. })
        ));
    }

    #[test]
    fn rejects_non_hex() {
        assert!(matches!(parse_text("zzzz"), Err(ConvertError::InvalidHex)));
    }

    #[test]
    fn uuid_nil_and_max_parse_correctly() {
        // Explicit anchors for the boundary UUIDs. The round-trip
        // property `uuid_parse_all_formats_round_trip` already exercises
        // the parser exhaustively; these cases nail the well-known
        // values themselves.
        let nil_text = "00000000-0000-0000-0000-000000000000";
        let nil = parse_text(nil_text).unwrap();
        assert_eq!(nil, Uuid::nil());
        assert_eq!(to_text(nil), nil_text);
        assert_eq!(to_bytes(nil), [0u8; 16]);

        let max_text = "ffffffff-ffff-ffff-ffff-ffffffffffff";
        let max = parse_text(max_text).unwrap();
        assert_eq!(max, Uuid::max());
        assert_eq!(to_text(max), max_text);
        assert_eq!(to_bytes(max), [0xff; 16]);
    }

    #[test]
    fn parse_text_braced_empty_rejected() {
        assert!(matches!(
            parse_text("{}"),
            Err(ConvertError::InvalidUuid { .. })
        ));
    }

    #[test]
    fn parse_text_only_dashes_rejected() {
        let s = "-".repeat(36);
        assert!(matches!(
            parse_text(&s),
            Err(ConvertError::InvalidUuid { .. })
        ));
    }

    #[test]
    fn parse_text_31_hex_digits_rejected() {
        let s = "a".repeat(31);
        assert!(matches!(
            parse_text(&s),
            Err(ConvertError::InvalidUuid { .. })
        ));
    }

    #[test]
    fn parse_text_with_extra_chars_after_36_hex_digits_rejected() {
        // 36 hex chars (no dashes) → exceeds the 32 hex digits the loop accepts.
        let s = "a".repeat(36);
        assert!(matches!(
            parse_text(&s),
            Err(ConvertError::InvalidUuid { .. })
        ));
    }

    #[test]
    fn from_bytes_zero_length_rejected() {
        assert!(matches!(
            from_bytes(&[]),
            Err(ConvertError::Length {
                expected: 16,
                got: 0
            })
        ));
    }

    #[test]
    fn from_bytes_17_bytes_rejected() {
        assert!(matches!(
            from_bytes(&[0u8; 17]),
            Err(ConvertError::Length {
                expected: 16,
                got: 17
            })
        ));
    }

    #[test]
    fn rejects_wrong_byte_length() {
        assert!(matches!(
            from_bytes(&[0u8; 8]),
            Err(ConvertError::Length {
                expected: 16,
                got: 8
            })
        ));
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    fn any_uuid() -> impl Strategy<Value = Uuid> {
        any::<[u8; 16]>().prop_map(Uuid::from_bytes)
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn uuid_text_36_round_trip(#[strategy(any_uuid())] u: Uuid) {
        let text = to_text(u);
        let parsed = parse_text(&text).expect("parse");
        prop_assert_eq!(parsed, u);
    }

    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn uuid_binary_16_round_trip(#[strategy(any_uuid())] u: Uuid) {
        let raw = to_bytes(u);
        let back = from_bytes(&raw).expect("decode");
        prop_assert_eq!(back, u);
    }

    /// All three accepted text shapes — canonical hyphenated, 32-char
    /// hex-only, and the Microsoft `{…}`-braced variant — must parse
    /// back to the same UUID, and rendering must always produce the
    /// canonical form. The upper-case variant is exercised on the
    /// canonical-hyphenated shape (the input is intrinsically hex).
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn uuid_parse_all_formats_round_trip(#[strategy(any_uuid())] u: Uuid) {
        let canonical = to_text(u);
        let hex_only = canonical.replace('-', "");
        let braced = format!("{{{canonical}}}");
        let upper = canonical.to_ascii_uppercase();

        prop_assert_eq!(parse_text(&canonical).expect("canonical"), u);
        prop_assert_eq!(parse_text(&hex_only).expect("hex-only"), u);
        prop_assert_eq!(parse_text(&braced).expect("braced"), u);
        prop_assert_eq!(parse_text(&upper).expect("upper"), u);

        // Rendering is canonical (lower-case, hyphenated).
        prop_assert_eq!(to_text(u).len(), 36);
        prop_assert_eq!(to_text(u), canonical);
    }
}
