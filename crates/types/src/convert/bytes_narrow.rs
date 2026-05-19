//! `Bytes → Bytes` size-narrowing under `truncate=true`. Hard byte cut, no
//! UTF-8 awareness.

use super::error::ConvertError;
use crate::{DataType, Value};

pub fn convert(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let mut bytes = match value {
        Value::Bytes(b) => b,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    if let Some(max) = sink_size
        && bytes.len() > max as usize
    {
        bytes.truncate(max as usize);
    }
    Ok(Value::Bytes(bytes))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn value_shape_mismatch() {
        let res = convert(Value::Int32(1), &DataType::Bytes { size: None }, None);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn passthrough_when_size_none() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3, 4, 5]),
            &DataType::Bytes { size: None },
            None,
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3, 4, 5]));
    }

    #[test]
    fn truncates_to_max() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3, 4, 5]),
            &DataType::Bytes { size: Some(3) },
            Some(3),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn max_zero_yields_empty() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3]),
            &DataType::Bytes { size: Some(0) },
            Some(0),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![]));
    }

    #[test]
    fn exact_fit_passthrough() {
        let out = convert(
            Value::Bytes(vec![1, 2, 3]),
            &DataType::Bytes { size: Some(3) },
            Some(3),
        )
        .unwrap();
        assert_eq!(out, Value::Bytes(vec![1, 2, 3]));
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// `Bytes → Bytes` truncation is a HARD cut at the exact byte index
    /// — UTF-8 boundaries are deliberately NOT respected. This matches
    /// the spec for opaque byte columns (`bytea`, `BLOB`, etc.). The
    /// property pins:
    ///
    /// * when `len <= max`, the output equals the input;
    /// * when `len > max`, the output is exactly the first `max` bytes,
    ///   regardless of whether that index falls mid-codepoint had the
    ///   bytes been UTF-8.
    #[test_strategy::proptest]
    fn bytes_narrow_hard_cut_at_exact_index(
        #[strategy(prop::collection::vec(any::<u8>(), 0..32))] bytes: Vec<u8>,
        #[strategy(0u32..32)] max: u32,
    ) {
        let out = convert(
            Value::Bytes(bytes.clone()),
            &DataType::Bytes { size: Some(max) },
            Some(max),
        )
        .unwrap();
        let Value::Bytes(out_bytes) = out else {
            prop_assert!(false, "expected Value::Bytes");
            return Ok(());
        };
        if bytes.len() <= max as usize {
            prop_assert_eq!(out_bytes, bytes);
        } else {
            prop_assert_eq!(out_bytes.len(), max as usize);
            prop_assert_eq!(&out_bytes[..], &bytes[..max as usize]);
        }
    }

    /// Confirms the "no UTF-8 awareness" half of the contract: feed
    /// UTF-8 bytes from a multi-byte codepoint and cut mid-codepoint —
    /// the result is the raw byte prefix (which would be invalid UTF-8
    /// when viewed as text). This is the spec.
    #[test]
    fn bytes_narrow_does_not_respect_utf8_boundaries() {
        // "ñ" = 0xC3 0xB1 in UTF-8. Cut at byte 1 mid-codepoint.
        let input = "ñ".as_bytes().to_vec();
        assert_eq!(input.len(), 2);
        let out = convert(
            Value::Bytes(input),
            &DataType::Bytes { size: Some(1) },
            Some(1),
        )
        .unwrap();
        let Value::Bytes(bytes) = out else {
            panic!("expected Value::Bytes");
        };
        assert_eq!(bytes, vec![0xC3]);
        // The single byte 0xC3 alone is NOT valid UTF-8 — the cut was
        // made without UTF-8 awareness, by design.
        assert!(std::str::from_utf8(&bytes).is_err());
    }
}
