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
use crate::types::{DataType, Value};

const MAX_TOKEN_BYTES: usize = 5;

pub fn convert(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: *src }),
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

    #[test]
    fn truthy_tokens() {
        for s in [
            "y", "Y", "t", "T", "1", "true", "TRUE", "TrUe", "yes", "YES",
        ] {
            assert_eq!(parse(s), Some(true), "{s:?}");
        }
    }

    #[test]
    fn falsy_tokens() {
        for s in [
            "n", "N", "f", "F", "0", "false", "FALSE", "FaLsE", "no", "NO",
        ] {
            assert_eq!(parse(s), Some(false), "{s:?}");
        }
    }

    #[test]
    fn empty_rejected() {
        assert_eq!(parse(""), None);
    }

    #[test]
    fn unknown_rejected() {
        assert_eq!(parse("maybe"), None);
        assert_eq!(parse("2"), None);
        assert_eq!(parse("truee"), None);
    }

    #[test]
    fn whitespace_not_trimmed() {
        assert_eq!(parse(" yes "), None);
        assert_eq!(parse("yes "), None);
        assert_eq!(parse(" y"), None);
    }

    #[test]
    fn non_ascii_rejected() {
        assert_eq!(parse("é"), None);
        assert_eq!(parse("дa"), None);
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
}
