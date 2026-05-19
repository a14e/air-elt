//! `Text → Text` size-narrowing under `truncate=true`. Identity / pure
//! widening is handled by the dispatcher's short-circuit and never reaches
//! this module.

use super::error::ConvertError;
use super::text_truncate::truncate_chars;
use crate::{DataType, Value};

pub fn convert(
    value: Value,
    src: &DataType,
    sink_size: Option<u32>,
) -> Result<Value, ConvertError> {
    let s = match value {
        Value::Text(s) => s,
        _ => return Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
    };
    let out = match sink_size {
        None => s,
        Some(max) => truncate_chars(&s, max as usize).to_string(),
    };
    Ok(Value::Text(out))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn value_shape_mismatch() {
        let res = convert(Value::Int32(1), &DataType::Text { size: None }, None);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn passthrough_when_size_none() {
        let out = convert(
            Value::Text("hello".into()),
            &DataType::Text { size: None },
            None,
        )
        .unwrap();
        assert_eq!(out, Value::Text("hello".into()));
    }

    #[test]
    fn truncates_to_max_chars() {
        // Multibyte input: each char is 2 bytes in UTF-8 but counts as 1 char.
        let out = convert(
            Value::Text("привет".into()),
            &DataType::Text { size: Some(3) },
            Some(3),
        )
        .unwrap();
        assert_eq!(out, Value::Text("при".into()));
    }

    #[test]
    fn max_zero_yields_empty() {
        let out = convert(
            Value::Text("abc".into()),
            &DataType::Text { size: Some(0) },
            Some(0),
        )
        .unwrap();
        assert_eq!(out, Value::Text(String::new()));
    }

    #[test]
    fn exact_char_fit() {
        let out = convert(
            Value::Text("abc".into()),
            &DataType::Text { size: Some(3) },
            Some(3),
        )
        .unwrap();
        assert_eq!(out, Value::Text("abc".into()));
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    fn arb_unicode_string() -> impl Strategy<Value = String> {
        prop::collection::vec(any::<char>(), 0..20)
            .prop_map(|cs| cs.into_iter().collect::<String>())
    }

    /// Random Unicode strings truncated to any character budget always
    /// produce a valid UTF-8 string. Rust's `&str` invariant already
    /// guarantees this at the type level (the slice we hand out came
    /// from a `char_indices` boundary), but the property also asserts
    /// codepoint-count semantics and prefix-ness — independent of the
    /// type system.
    #[test_strategy::proptest]
    fn text_narrow_utf8_char_boundaries(
        #[strategy(arb_unicode_string())] s: String,
        #[strategy(0u32..40)] max: u32,
    ) {
        let out = convert(
            Value::Text(s.clone()),
            &DataType::Text { size: Some(max) },
            Some(max),
        )
        .unwrap();
        let Value::Text(out_s) = out else {
            prop_assert!(false, "expected Value::Text");
            return Ok(());
        };
        // The result roundtrips through `from_utf8` — i.e. it is valid
        // UTF-8 at every byte boundary.
        prop_assert!(std::str::from_utf8(out_s.as_bytes()).is_ok());
        // Codepoint-count semantics: result has at most `max` chars.
        prop_assert!(out_s.chars().count() <= max as usize);
        // And is a prefix of the input.
        prop_assert!(s.starts_with(&out_s));
        // Either we truncated at exactly the budget, or we returned the
        // full input.
        let input_chars = s.chars().count();
        if input_chars > max as usize {
            prop_assert_eq!(out_s.chars().count(), max as usize);
        } else {
            prop_assert_eq!(out_s.as_str(), s.as_str());
        }
    }
}
