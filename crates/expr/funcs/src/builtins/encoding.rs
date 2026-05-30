use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};
use encoding_rs::Encoding;

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static ENCODE: EncodeFunc = EncodeFunc;
static DECODE: DecodeFunc = DecodeFunc;
static ENCODE_TEXT: EncodeTextFunc = EncodeTextFunc;
static DECODE_TEXT: DecodeTextFunc = DecodeTextFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&ENCODE);
    registry.register(&DECODE);
    registry.register(&ENCODE_TEXT);
    registry.register(&DECODE_TEXT);
}

/// Valid binary-to-text algorithm labels for `encode` / `decode`. Shared by
/// both `evaluate` and `validate_const_args` so the accepted set never drifts.
const ENCODE_ALGORITHMS: [&str; 3] = ["hex", "base64", "base64url"];

/// Builds the "unknown algorithm" error used wherever an unrecognised
/// `encode`/`decode` label is rejected.
fn unknown_algorithm_error(algorithm: &str) -> FuncError {
    FuncError::EncodingError {
        reason: format!("unknown algorithm: {algorithm} (expected hex, base64, or base64url)"),
    }
}

/// Validates a constant algorithm label (arg index 1) for `encode`/`decode`.
/// Dynamic or non-`Text` labels are skipped.
fn validate_encode_algorithm_const(args: &[Option<&Value>]) -> Result<(), FuncError> {
    if let Some(Some(Value::Text(algorithm))) = args.get(1) {
        if !ENCODE_ALGORITHMS.contains(&algorithm.as_str()) {
            return Err(unknown_algorithm_error(algorithm));
        }
    }
    Ok(())
}

/// Validates a constant `encoding_rs` label (arg index 1) for
/// `encodeText`/`decodeText`. Dynamic or non-`Text` labels are skipped.
fn validate_label_const(function: &str, args: &[Option<&Value>]) -> Result<(), FuncError> {
    if let Some(Some(Value::Text(label))) = args.get(1) {
        if Encoding::for_label(label.as_bytes()).is_none() {
            return Err(FuncError::EncodingError {
                reason: format!("unknown encoding: {label} (in {function})"),
            });
        }
    }
    Ok(())
}

fn extract_bytes(val: Value, func_name: &str) -> Result<Vec<u8>, FuncError> {
    match val {
        Value::Bytes(b) => Ok(b),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Bytes".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_text(val: Value, func_name: &str) -> Result<String, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

// --- encode ---

struct EncodeFunc;

impl ExprFunction for EncodeFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "encode"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("encode", &args[0].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let algorithm_val = args.remove(1);
        let bytes_val = args.remove(0);
        if bytes_val.is_null() || algorithm_val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(bytes_val, "encode")?;
        let algorithm = extract_text(algorithm_val, "encode")?;
        let encoded = match algorithm.as_str() {
            "hex" => hex::encode(&bytes),
            "base64" => BASE64_STANDARD.encode(&bytes),
            "base64url" => URL_SAFE_NO_PAD.encode(&bytes),
            other => return Err(unknown_algorithm_error(other)),
        };
        Ok(Value::Text(encoded))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_encode_algorithm_const(args)
    }
}

// --- decode ---

struct DecodeFunc;

impl ExprFunction for DecodeFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "decode"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("decode", &args[0].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Bytes { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let algorithm_val = args.remove(1);
        let text_val = args.remove(0);
        if text_val.is_null() || algorithm_val.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text(text_val, "decode")?;
        let algorithm = extract_text(algorithm_val, "decode")?;
        let decoded =
            match algorithm.as_str() {
                "hex" => hex::decode(&text).map_err(|e| FuncError::EncodingError {
                    reason: format!("hex decode failed: {e}"),
                })?,
                "base64" => BASE64_STANDARD.decode(text.as_bytes()).map_err(|e| {
                    FuncError::EncodingError {
                        reason: format!("base64 decode failed: {e}"),
                    }
                })?,
                "base64url" => URL_SAFE_NO_PAD.decode(text.as_bytes()).map_err(|e| {
                    FuncError::EncodingError {
                        reason: format!("base64url decode failed: {e}"),
                    }
                })?,
                other => return Err(unknown_algorithm_error(other)),
            };
        Ok(Value::Bytes(decoded))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_encode_algorithm_const(args)
    }
}

// --- encodeText ---

struct EncodeTextFunc;

impl ExprFunction for EncodeTextFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "encodeText"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("encodeText", &args[0].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Bytes { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let encoding_val = args.remove(1);
        let text_val = args.remove(0);
        if text_val.is_null() || encoding_val.is_null() {
            return Ok(Value::Null);
        }
        let text = extract_text(text_val, "encodeText")?;
        let encoding_name = extract_text(encoding_val, "encodeText")?;

        let enc = Encoding::for_label(encoding_name.as_bytes()).ok_or_else(|| {
            FuncError::EncodingError {
                reason: format!("unknown encoding: {encoding_name}"),
            }
        })?;

        let (bytes, _, had_errors) = enc.encode(&text);
        if had_errors {
            return Err(FuncError::EncodingError {
                reason: format!(
                    "encoding to {encoding_name} failed: unmappable characters in input"
                ),
            });
        }
        Ok(Value::Bytes(bytes.into_owned()))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_label_const("encodeText", args)
    }
}

// --- decodeText ---

struct DecodeTextFunc;

impl ExprFunction for DecodeTextFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "decodeText"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("decodeText", &args[0].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let encoding_val = args.remove(1);
        let bytes_val = args.remove(0);
        if bytes_val.is_null() || encoding_val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(bytes_val, "decodeText")?;
        let encoding_name = extract_text(encoding_val, "decodeText")?;

        let enc = Encoding::for_label(encoding_name.as_bytes()).ok_or_else(|| {
            FuncError::EncodingError {
                reason: format!("unknown encoding: {encoding_name}"),
            }
        })?;

        let (text, _, had_errors) = enc.decode(&bytes);
        if had_errors {
            return Err(FuncError::EncodingError {
                reason: format!(
                    "decoding from {encoding_name} failed: malformed byte sequence in input"
                ),
            });
        }
        Ok(Value::Text(text.into_owned()))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        validate_label_const("decodeText", args)
    }
}

fn validate_bytes_arg(function: &str, dt: &DataType) -> Result<(), FuncError> {
    if !matches!(dt, DataType::Bytes { .. }) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "Bytes".to_owned(),
            actual: format!("{dt}"),
        });
    }
    Ok(())
}

fn validate_text_arg(function: &str, dt: &DataType) -> Result<(), FuncError> {
    if !matches!(dt, DataType::Text { .. }) {
        return Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{dt}"),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    #[test]
    fn encode_hex() {
        let result = EncodeFunc
            .evaluate(
                vec![Value::Bytes(vec![0xCA, 0xFE]), Value::Text("hex".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("cafe".into()));
    }

    #[test]
    fn decode_hex() {
        let result = DecodeFunc
            .evaluate(
                vec![Value::Text("cafe".into()), Value::Text("hex".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bytes(vec![0xCA, 0xFE]));
    }

    #[test]
    fn encode_base64() {
        let result = EncodeFunc
            .evaluate(
                vec![Value::Bytes(vec![1, 2, 3]), Value::Text("base64".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("AQID".into()));
    }

    #[test]
    fn decode_base64() {
        let result = DecodeFunc
            .evaluate(
                vec![Value::Text("AQID".into()), Value::Text("base64".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bytes(vec![1, 2, 3]));
    }

    #[test]
    fn encode_base64url() {
        // Bytes that produce different output for standard vs URL-safe base64
        let result = EncodeFunc
            .evaluate(
                vec![
                    Value::Bytes(vec![0xFB, 0xFF, 0xFE]),
                    Value::Text("base64url".into()),
                ],
                &ctx(),
            )
            .unwrap();
        // URL-safe uses - and _ instead of + and /, no padding
        let text = match result {
            Value::Text(s) => s,
            _ => panic!("expected Text"),
        };
        assert!(!text.contains('+'));
        assert!(!text.contains('/'));
        assert!(!text.contains('='));
    }

    #[test]
    fn decode_base64url() {
        // Encode then decode roundtrip
        let original = vec![0xFB, 0xFF, 0xFE];
        let encoded = EncodeFunc
            .evaluate(
                vec![
                    Value::Bytes(original.clone()),
                    Value::Text("base64url".into()),
                ],
                &ctx(),
            )
            .unwrap();
        let encoded_text = match encoded {
            Value::Text(s) => s,
            _ => panic!("expected Text"),
        };
        let decoded = DecodeFunc
            .evaluate(
                vec![Value::Text(encoded_text), Value::Text("base64url".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(decoded, Value::Bytes(original));
    }

    #[test]
    fn encode_unknown_algorithm() {
        let result = EncodeFunc.evaluate(
            vec![Value::Bytes(vec![1, 2, 3]), Value::Text("rot13".into())],
            &ctx(),
        );
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("unknown algorithm"));
    }

    #[test]
    fn encode_text_utf8() {
        let result = EncodeTextFunc
            .evaluate(
                vec![Value::Text("hello".into()), Value::Text("utf-8".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn encode_text_windows_1251() {
        // Russian letter "A" (U+0410) in windows-1251 is 0xC0
        let result = EncodeTextFunc
            .evaluate(
                vec![
                    Value::Text("\u{0410}".into()),
                    Value::Text("windows-1251".into()),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bytes(vec![0xC0]));
    }

    #[test]
    fn decode_text_utf8() {
        let result = DecodeTextFunc
            .evaluate(
                vec![Value::Bytes(b"hello".to_vec()), Value::Text("utf-8".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("hello".into()));
    }

    #[test]
    fn decode_text_windows_1251() {
        // 0xC0 in windows-1251 is Russian letter "A" (U+0410)
        let result = DecodeTextFunc
            .evaluate(
                vec![Value::Bytes(vec![0xC0]), Value::Text("windows-1251".into())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Text("\u{0410}".into()));
    }

    #[test]
    fn unknown_encoding_error() {
        let result = EncodeTextFunc.evaluate(
            vec![
                Value::Text("test".into()),
                Value::Text("nonsense-encoding".into()),
            ],
            &ctx(),
        );
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("unknown encoding"));
    }

    #[test]
    fn null_propagation_encode() {
        let result = EncodeFunc
            .evaluate(vec![Value::Null, Value::Text("hex".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_decode_text() {
        let result = DecodeTextFunc
            .evaluate(vec![Value::Null, Value::Text("utf-8".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn validate_const_encode_valid_ok() {
        let algorithm = Value::Text("base64".into());
        let result = EncodeFunc.validate_const_args(&[None, Some(&algorithm)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_encode_dynamic_ok() {
        let result = DecodeFunc.validate_const_args(&[None, None], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_encode_invalid_errors() {
        let algorithm = Value::Text("rot13".into());
        let result = EncodeFunc.validate_const_args(&[None, Some(&algorithm)], &ctx());
        assert!(matches!(result, Err(FuncError::EncodingError { .. })));
    }

    #[test]
    fn validate_const_label_valid_ok() {
        let label = Value::Text("windows-1251".into());
        let result = EncodeTextFunc.validate_const_args(&[None, Some(&label)], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn validate_const_label_invalid_errors() {
        let label = Value::Text("nonsense-encoding".into());
        let result = DecodeTextFunc.validate_const_args(&[None, Some(&label)], &ctx());
        assert!(matches!(result, Err(FuncError::EncodingError { .. })));
    }
}
