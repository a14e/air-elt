use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD as BASE64_STANDARD, URL_SAFE_NO_PAD};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static BYTE_LENGTH: ByteLengthFunc = ByteLengthFunc;
static BYTE_AT: ByteAtFunc = ByteAtFunc;
static BYTE_SLICE: ByteSliceFunc = ByteSliceFunc;
static BYTES_FROM_HEX: BytesFromHexFunc = BytesFromHexFunc;
static BYTES_FROM_BASE64: BytesFromBase64Func = BytesFromBase64Func;
static BYTES_FROM_UTF8: BytesFromUtf8Func = BytesFromUtf8Func;
static HEX: HexFunc = HexFunc;
static BASE64: Base64Func = Base64Func;
static BASE64_URL: Base64UrlFunc = Base64UrlFunc;
static BYTES_EQUAL: BytesEqualFunc = BytesEqualFunc;
static URL_ENCODE: UrlEncodeFunc = UrlEncodeFunc;
static URL_DECODE: UrlDecodeFunc = UrlDecodeFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&BYTE_LENGTH);
    registry.register(&BYTE_AT);
    registry.register(&BYTE_SLICE);
    registry.register(&BYTES_FROM_HEX);
    registry.register(&BYTES_FROM_BASE64);
    registry.register(&BYTES_FROM_UTF8);
    registry.register(&HEX);
    registry.register(&BASE64);
    registry.register(&BASE64_URL);
    registry.register(&BYTES_EQUAL);
    registry.register(&URL_ENCODE);
    registry.register(&URL_DECODE);
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

fn extract_int64(val: Value, func_name: &str) -> Result<i64, FuncError> {
    match val {
        Value::Int64(n) => Ok(n),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Int64".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

// --- byteLength ---

struct ByteLengthFunc;

impl ExprFunction for ByteLengthFunc {
    fn name(&self) -> &str {
        "byteLength"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("byteLength", &args[0].data_type)?;
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(val, "byteLength")?;
        Ok(Value::Int64(bytes.len() as i64))
    }
}

// --- byteAt ---

struct ByteAtFunc;

impl ExprFunction for ByteAtFunc {
    fn name(&self) -> &str {
        "byteAt"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("byteAt", &args[0].data_type)?;
        Ok(NullableExprType::nullable(DataType::Int64))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let idx_val = args.remove(1);
        let bytes_val = args.remove(0);
        if bytes_val.is_null() || idx_val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(bytes_val, "byteAt")?;
        let idx = extract_int64(idx_val, "byteAt")?;
        if idx < 0 {
            return Ok(Value::Null);
        }
        let idx = idx as usize;
        match bytes.get(idx) {
            Some(&b) => Ok(Value::Int64(i64::from(b))),
            None => Ok(Value::Null),
        }
    }
}

// --- byteSlice ---

struct ByteSliceFunc;

impl ExprFunction for ByteSliceFunc {
    fn name(&self) -> &str {
        "byteSlice"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("byteSlice", &args[0].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Bytes { size: None },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let len_val = if args.len() == 3 {
            Some(args.remove(2))
        } else {
            None
        };
        let start_val = args.remove(1);
        let bytes_val = args.remove(0);

        if bytes_val.is_null() || start_val.is_null() {
            return Ok(Value::Null);
        }
        if let Some(ref lv) = len_val {
            if lv.is_null() {
                return Ok(Value::Null);
            }
        }

        let mut bytes = extract_bytes(bytes_val, "byteSlice")?;
        let start = extract_int64(start_val, "byteSlice")?;
        let max_len = match len_val {
            Some(v) => Some(extract_int64(v, "byteSlice")?),
            None => None,
        };

        let start = if start < 0 { 0usize } else { start as usize };

        if start >= bytes.len() {
            return Ok(Value::Bytes(Vec::new()));
        }

        // In-place truncation: drain prefix, then truncate
        if start > 0 {
            bytes.drain(..start);
        }

        if let Some(len) = max_len {
            let len = if len < 0 { 0usize } else { len as usize };
            bytes.truncate(len);
        }

        Ok(Value::Bytes(bytes))
    }
}

// --- bytesFromHex ---

struct BytesFromHexFunc;

impl ExprFunction for BytesFromHexFunc {
    fn name(&self) -> &str {
        "bytesFromHex"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("bytesFromHex", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Bytes { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(val, "bytesFromHex")?;
        let bytes = hex::decode(&s).map_err(|e| FuncError::EncodingError {
            reason: format!("hex decode failed: {e}"),
        })?;
        Ok(Value::Bytes(bytes))
    }
}

// --- bytesFromBase64 ---

struct BytesFromBase64Func;

impl ExprFunction for BytesFromBase64Func {
    fn name(&self) -> &str {
        "bytesFromBase64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("bytesFromBase64", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Bytes { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(val, "bytesFromBase64")?;
        let bytes = BASE64_STANDARD
            .decode(s.as_bytes())
            .map_err(|e| FuncError::EncodingError {
                reason: format!("base64 decode failed: {e}"),
            })?;
        Ok(Value::Bytes(bytes))
    }
}

// --- bytesFromUtf8 ---

struct BytesFromUtf8Func;

impl ExprFunction for BytesFromUtf8Func {
    fn name(&self) -> &str {
        "bytesFromUtf8"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("bytesFromUtf8", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Bytes { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(val, "bytesFromUtf8")?;
        Ok(Value::Bytes(s.into_bytes()))
    }
}

// --- hex ---

struct HexFunc;

impl ExprFunction for HexFunc {
    fn name(&self) -> &str {
        "hex"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("hex", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(val, "hex")?;
        Ok(Value::Text(hex::encode(&bytes)))
    }
}

// --- base64 ---

struct Base64Func;

impl ExprFunction for Base64Func {
    fn name(&self) -> &str {
        "base64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("base64", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(val, "base64")?;
        Ok(Value::Text(BASE64_STANDARD.encode(&bytes)))
    }
}

// --- base64Url ---

struct Base64UrlFunc;

impl ExprFunction for Base64UrlFunc {
    fn name(&self) -> &str {
        "base64Url"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("base64Url", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes(val, "base64Url")?;
        Ok(Value::Text(URL_SAFE_NO_PAD.encode(&bytes)))
    }
}

// --- bytesEqual ---

struct BytesEqualFunc;

impl ExprFunction for BytesEqualFunc {
    fn name(&self) -> &str {
        "bytesEqual"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_bytes_arg("bytesEqual", &args[0].data_type)?;
        validate_bytes_arg("bytesEqual", &args[1].data_type)?;
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let b_val = args.remove(1);
        let a_val = args.remove(0);
        if a_val.is_null() || b_val.is_null() {
            return Ok(Value::Null);
        }
        let a = extract_bytes(a_val, "bytesEqual")?;
        let b = extract_bytes(b_val, "bytesEqual")?;
        Ok(Value::Bool(a == b))
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

struct UrlEncodeFunc;

impl ExprFunction for UrlEncodeFunc {
    fn name(&self) -> &str {
        "urlEncode"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("urlEncode", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(val, "urlEncode")?;
        Ok(Value::Text(urlencoding::encode(&s).into_owned()))
    }
}

struct UrlDecodeFunc;

impl ExprFunction for UrlDecodeFunc {
    fn name(&self) -> &str {
        "urlDecode"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        validate_text_arg("urlDecode", &args[0].data_type)?;
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            args[0].nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let s = extract_text(val, "urlDecode")?;
        let decoded = urlencoding::decode(&s).map_err(|e| FuncError::EncodingError {
            reason: format!("URL decode failed: {e}"),
        })?;
        Ok(Value::Text(decoded.into_owned()))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    #[test]
    fn hex_roundtrip() {
        let original = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let encoded = HexFunc
            .evaluate(vec![Value::Bytes(original.clone())], &ctx())
            .unwrap();
        assert_eq!(encoded, Value::Text("deadbeef".into()));

        let decoded = BytesFromHexFunc
            .evaluate(vec![Value::Text("deadbeef".into())], &ctx())
            .unwrap();
        assert_eq!(decoded, Value::Bytes(original));
    }

    #[test]
    fn base64_roundtrip() {
        let original = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let encoded = Base64Func
            .evaluate(vec![Value::Bytes(original.clone())], &ctx())
            .unwrap();
        assert_eq!(encoded, Value::Text("AQIDBAU=".into()));

        let decoded = BytesFromBase64Func
            .evaluate(vec![Value::Text("AQIDBAU=".into())], &ctx())
            .unwrap();
        assert_eq!(decoded, Value::Bytes(original));
    }

    #[test]
    fn base64_url_no_padding() {
        let original = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let encoded = Base64UrlFunc
            .evaluate(vec![Value::Bytes(original)], &ctx())
            .unwrap();
        // URL-safe base64 with no padding
        assert_eq!(encoded, Value::Text("AQIDBAU".into()));
    }

    #[test]
    fn byte_length_basic() {
        let result = ByteLengthFunc
            .evaluate(vec![Value::Bytes(vec![1, 2, 3, 4, 5])], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn byte_length_empty() {
        let result = ByteLengthFunc
            .evaluate(vec![Value::Bytes(vec![])], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(0));
    }

    #[test]
    fn byte_at_valid_index() {
        let result = ByteAtFunc
            .evaluate(
                vec![Value::Bytes(vec![10, 20, 30]), Value::Int64(1)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Int64(20));
    }

    #[test]
    fn byte_at_out_of_bounds() {
        let result = ByteAtFunc
            .evaluate(
                vec![Value::Bytes(vec![10, 20, 30]), Value::Int64(10)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn byte_at_negative_index() {
        let result = ByteAtFunc
            .evaluate(
                vec![Value::Bytes(vec![10, 20, 30]), Value::Int64(-1)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn byte_slice_with_length() {
        let result = ByteSliceFunc
            .evaluate(
                vec![
                    Value::Bytes(vec![1, 2, 3, 4, 5]),
                    Value::Int64(1),
                    Value::Int64(3),
                ],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bytes(vec![2, 3, 4]));
    }

    #[test]
    fn byte_slice_without_length() {
        let result = ByteSliceFunc
            .evaluate(
                vec![Value::Bytes(vec![1, 2, 3, 4, 5]), Value::Int64(2)],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bytes(vec![3, 4, 5]));
    }

    #[test]
    fn byte_slice_start_beyond_end() {
        let result = ByteSliceFunc
            .evaluate(vec![Value::Bytes(vec![1, 2, 3]), Value::Int64(10)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bytes(vec![]));
    }

    #[test]
    fn bytes_from_utf8_basic() {
        let result = BytesFromUtf8Func
            .evaluate(vec![Value::Text("hello".into())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn bytes_equal_true() {
        let result = BytesEqualFunc
            .evaluate(
                vec![Value::Bytes(vec![1, 2, 3]), Value::Bytes(vec![1, 2, 3])],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn bytes_equal_false() {
        let result = BytesEqualFunc
            .evaluate(
                vec![Value::Bytes(vec![1, 2, 3]), Value::Bytes(vec![4, 5, 6])],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn null_propagation_byte_length() {
        let result = ByteLengthFunc.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_hex() {
        let result = HexFunc.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_bytes_equal() {
        let result = BytesEqualFunc
            .evaluate(vec![Value::Bytes(vec![1, 2, 3]), Value::Null], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn null_propagation_byte_slice() {
        let result = ByteSliceFunc
            .evaluate(vec![Value::Null, Value::Int64(0)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }
}
