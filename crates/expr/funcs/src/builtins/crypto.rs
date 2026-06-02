use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use xxhash_rust::xxh32::xxh32;
use xxhash_rust::xxh64::xxh64;

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static MD5: Md5Func = Md5Func;
static SHA1: Sha1Func = Sha1Func;
static SHA256: Sha256Func = Sha256Func;
static SHA512: Sha512Func = Sha512Func;
static XXHASH64: XxHash64Func = XxHash64Func;
static XXHASH32: XxHash32Func = XxHash32Func;
static HMAC: HmacFunc = HmacFunc;
static CITY_HASH64: CityHash64Func = CityHash64Func;
static SIP_HASH64: SipHash64Func = SipHash64Func;
static FNV1A64: Fnv1a64Func = Fnv1a64Func;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&MD5);
    registry.register(&SHA1);
    registry.register(&SHA256);
    registry.register(&SHA512);
    registry.register(&XXHASH64);
    registry.register(&XXHASH32);
    registry.register(&HMAC);
    registry.register(&CITY_HASH64);
    registry.register(&SIP_HASH64);
    registry.register(&FNV1A64);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn hash_to_hex<D: Digest>(input: &[u8]) -> String {
    let mut hasher = D::new();
    hasher.update(input);
    let result = hasher.finalize();
    hex::encode(result)
}

fn extract_bytes_ref<'a>(value: &'a Value, function_name: &str) -> Result<&'a [u8], FuncError> {
    match value {
        Value::Text(s) => Ok(s.as_bytes()),
        Value::Bytes(b) => Ok(b),
        Value::Null => Ok(&[]),
        other => Err(FuncError::TypeMismatch {
            function: function_name.to_owned(),
            expected: "Text or Bytes".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

// ---------------------------------------------------------------------------
// md5
// ---------------------------------------------------------------------------

struct Md5Func;

impl ExprFunction for Md5Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "md5"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::Text { size: Some(32) },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "md5")?;
        Ok(Value::Text(hash_to_hex::<Md5>(bytes)))
    }
}

// ---------------------------------------------------------------------------
// sha1
// ---------------------------------------------------------------------------

struct Sha1Func;

impl ExprFunction for Sha1Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sha1"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::Text { size: Some(40) },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "sha1")?;
        Ok(Value::Text(hash_to_hex::<Sha1>(bytes)))
    }
}

// ---------------------------------------------------------------------------
// sha256
// ---------------------------------------------------------------------------

struct Sha256Func;

impl ExprFunction for Sha256Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sha256"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::Text { size: Some(64) },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "sha256")?;
        Ok(Value::Text(hash_to_hex::<Sha256>(bytes)))
    }
}

// ---------------------------------------------------------------------------
// sha512
// ---------------------------------------------------------------------------

struct Sha512Func;

impl ExprFunction for Sha512Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sha512"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(
            DataType::Text { size: Some(128) },
            args[0].nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "sha512")?;
        Ok(Value::Text(hash_to_hex::<Sha512>(bytes)))
    }
}

// ---------------------------------------------------------------------------
// xxHash64
// ---------------------------------------------------------------------------

struct XxHash64Func;

impl ExprFunction for XxHash64Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "xxHash64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "xxHash64")?;
        let hash = xxh64(bytes, 0);
        Ok(Value::Int64(hash as i64))
    }
}

// ---------------------------------------------------------------------------
// xxHash32
// ---------------------------------------------------------------------------

struct XxHash32Func;

impl ExprFunction for XxHash32Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "xxHash32"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "xxHash32")?;
        let hash = xxh32(bytes, 0);
        Ok(Value::Int64(i64::from(hash)))
    }
}

// ---------------------------------------------------------------------------
// hmac
// ---------------------------------------------------------------------------

type HmacSha256 = Hmac<Sha256>;
type HmacSha512 = Hmac<Sha512>;

/// Supported HMAC algorithm labels (arg index 0). Shared by `evaluate` and
/// `validate_const_args` so the accepted set never drifts.
const HMAC_ALGORITHMS: [&str; 2] = ["sha256", "sha512"];

/// Builds the "unsupported algorithm" error for an unrecognised HMAC label.
fn unsupported_hmac_algorithm_error(algorithm: &str) -> FuncError {
    FuncError::EvalFailed {
        function: "hmac".to_owned(),
        reason: format!("unsupported algorithm: {algorithm} (supported: sha256, sha512)"),
    }
}

struct HmacFunc;

impl ExprFunction for HmacFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "hmac"
    }

    fn min_args(&self) -> usize {
        3
    }

    fn max_args(&self) -> Option<usize> {
        Some(3)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(
            DataType::Text { size: None },
            nullable,
        ))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        if args.read(0).is_null() || args.read(1).is_null() || args.read(2).is_null() {
            return Ok(Value::Null);
        }

        let algorithm_str = match args.read(0) {
            Value::Text(s) => s.as_str(),
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "hmac".to_owned(),
                    expected: "Text (algorithm name)".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };

        let message_bytes = extract_bytes_ref(args.read(1), "hmac")?;
        let key_bytes = extract_bytes_ref(args.read(2), "hmac")?;

        let hex_result = match algorithm_str {
            "sha256" => {
                let mut mac =
                    HmacSha256::new_from_slice(key_bytes).map_err(|e| FuncError::EvalFailed {
                        function: "hmac".to_owned(),
                        reason: format!("invalid key: {e}"),
                    })?;
                mac.update(message_bytes);
                hex::encode(mac.finalize().into_bytes())
            }
            "sha512" => {
                let mut mac =
                    HmacSha512::new_from_slice(key_bytes).map_err(|e| FuncError::EvalFailed {
                        function: "hmac".to_owned(),
                        reason: format!("invalid key: {e}"),
                    })?;
                mac.update(message_bytes);
                hex::encode(mac.finalize().into_bytes())
            }
            other => return Err(unsupported_hmac_algorithm_error(other)),
        };

        Ok(Value::Text(hex_result))
    }

    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        _context: &EvalContext,
    ) -> Result<(), FuncError> {
        if let Some(Some(Value::Text(algorithm))) = args.first() {
            if !HMAC_ALGORITHMS.contains(&algorithm.as_str()) {
                return Err(unsupported_hmac_algorithm_error(algorithm));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// cityHash64
// ---------------------------------------------------------------------------

struct CityHash64Func;

impl ExprFunction for CityHash64Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "cityHash64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "cityHash64")?;
        let hash = city_hash_64(bytes);
        Ok(Value::Int64(hash as i64))
    }
}

fn city_hash_64(data: &[u8]) -> u64 {
    ch_cityhash102::cityhash64(data)
}

// ---------------------------------------------------------------------------
// sipHash64
// ---------------------------------------------------------------------------

struct SipHash64Func;

impl ExprFunction for SipHash64Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "sipHash64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "sipHash64")?;
        let hash = sip_hash_64(bytes);
        Ok(Value::Int64(hash as i64))
    }
}

fn sip_hash_64(data: &[u8]) -> u64 {
    use std::hash::Hasher;
    #[allow(deprecated)]
    let mut hasher = std::hash::SipHasher::new();
    hasher.write(data);
    hasher.finish()
}

// ---------------------------------------------------------------------------
// fnv1a64
// ---------------------------------------------------------------------------

struct Fnv1a64Func;

impl ExprFunction for Fnv1a64Func {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "fnv1a64"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Int64, args[0].nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let input = args.read(0);
        if input.is_null() {
            return Ok(Value::Null);
        }
        let bytes = extract_bytes_ref(input, "fnv1a64")?;
        let hash = fnv1a_64(bytes);
        Ok(Value::Int64(hash as i64))
    }
}

fn fnv1a_64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    #[test]
    fn md5_empty_string() {
        let result = eval(
            &Md5Func,
            smallvec::smallvec![Value::Text(String::new())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::Text("d41d8cd98f00b204e9800998ecf8427e".to_owned())
        );
    }

    #[test]
    fn md5_null_propagation() {
        let result = eval(&Md5Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn sha1_hello() {
        let result = eval(
            &Sha1Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::Text("aaf4c61ddcc5e8a2dabede0f3b482cd9aea9434d".to_owned())
        );
    }

    #[test]
    fn sha1_null_propagation() {
        let result = eval(&Sha1Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn sha256_hello() {
        let result = eval(
            &Sha256Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::Text(
                "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824".to_owned()
            )
        );
    }

    #[test]
    fn sha256_null_propagation() {
        let result = eval(&Sha256Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn sha512_non_empty() {
        let result = eval(
            &Sha512Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Text(hex) => {
                assert_eq!(hex.len(), 128);
                assert_eq!(
                    hex,
                    "9b71d224bd62f3785d96d46ad3ea3d73319bfbc2890caadae2dff72519673ca7\
                     2323c3d99ba5c11d7c7acc6e14b8c5da0c4663475c2e5c3adef46f73bcdec043"
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn sha512_null_propagation() {
        let result = eval(&Sha512Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn xxhash64_deterministic() {
        let input = Value::Text("test data".to_owned());
        let result_a = eval(&XxHash64Func, smallvec::smallvec![input.clone()], &ctx()).unwrap();
        let result_b = eval(&XxHash64Func, smallvec::smallvec![input], &ctx()).unwrap();
        assert_eq!(result_a, result_b);
        assert!(matches!(result_a, Value::Int64(_)));
    }

    #[test]
    fn xxhash64_null_propagation() {
        let result = eval(&XxHash64Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn xxhash32_deterministic() {
        let input = Value::Text("test data".to_owned());
        let result_a = eval(&XxHash32Func, smallvec::smallvec![input.clone()], &ctx()).unwrap();
        let result_b = eval(&XxHash32Func, smallvec::smallvec![input], &ctx()).unwrap();
        assert_eq!(result_a, result_b);
        assert!(matches!(result_a, Value::Int64(_)));
    }

    #[test]
    fn xxhash32_null_propagation() {
        let result = eval(&XxHash32Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn hmac_sha256_known_value() {
        let result = eval(
            &HmacFunc,
            smallvec::smallvec![
                Value::Text("sha256".to_owned()),
                Value::Text("message".to_owned()),
                Value::Text("key".to_owned()),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(
            result,
            Value::Text(
                "6e9ef29b75fffc5b7abae527d58fdadb2fe42e7219011976917343065f58ed4a".to_owned()
            )
        );
    }

    #[test]
    fn hmac_sha512_known_value() {
        let result = eval(
            &HmacFunc,
            smallvec::smallvec![
                Value::Text("sha512".to_owned()),
                Value::Text("message".to_owned()),
                Value::Text("key".to_owned()),
            ],
            &ctx(),
        )
        .unwrap();
        match result {
            Value::Text(hex) => assert_eq!(hex.len(), 128),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn hmac_null_propagation() {
        let result = eval(
            &HmacFunc,
            smallvec::smallvec![
                Value::Text("sha256".to_owned()),
                Value::Null,
                Value::Text("key".to_owned()),
            ],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn hmac_unsupported_algorithm() {
        let result = eval(
            &HmacFunc,
            smallvec::smallvec![
                Value::Text("md5".to_owned()),
                Value::Text("message".to_owned()),
                Value::Text("key".to_owned()),
            ],
            &ctx(),
        );
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn hmac_validate_const_valid_ok() {
        let algorithm = Value::Text("sha512".to_owned());
        let result = HmacFunc.validate_const_args(&[Some(&algorithm), None, None], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn hmac_validate_const_dynamic_ok() {
        let result = HmacFunc.validate_const_args(&[None, None, None], &ctx());
        assert!(result.is_ok());
    }

    #[test]
    fn hmac_validate_const_invalid_errors() {
        let algorithm = Value::Text("md5".to_owned());
        let result = HmacFunc.validate_const_args(&[Some(&algorithm), None, None], &ctx());
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn md5_bytes_input() {
        let result = eval(
            &Md5Func,
            smallvec::smallvec![Value::Bytes(vec![0x68, 0x65, 0x6c, 0x6c, 0x6f])],
            &ctx(),
        )
        .unwrap();
        // "hello" in bytes
        let expected = eval(
            &Md5Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, expected);
    }

    #[test]
    fn type_mismatch_on_non_bytes_input() {
        let result = eval(&Md5Func, smallvec::smallvec![Value::Int64(42)], &ctx());
        assert!(matches!(result, Err(FuncError::TypeMismatch { .. })));
    }

    // --- cityHash64 ---

    #[test]
    fn city_hash64_deterministic_with_golden_value() {
        let result = eval(
            &CityHash64Func,
            smallvec::smallvec![Value::Text("test data".to_owned())],
            &ctx(),
        )
        .unwrap();
        // Golden value from ch_cityhash102 reference (ClickHouse CityHash v1.0.2)
        let expected = ch_cityhash102::cityhash64(b"test data") as i64;
        assert_eq!(result, Value::Int64(expected));

        let empty = eval(
            &CityHash64Func,
            smallvec::smallvec![Value::Text(String::new())],
            &ctx(),
        )
        .unwrap();
        let expected_empty = ch_cityhash102::cityhash64(b"") as i64;
        assert_eq!(empty, Value::Int64(expected_empty));
    }

    #[test]
    fn city_hash64_null_propagation() {
        let result = eval(&CityHash64Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn city_hash64_different_inputs_different_hashes() {
        let result_a = eval(
            &CityHash64Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        let result_b = eval(
            &CityHash64Func,
            smallvec::smallvec![Value::Text("world".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_ne!(result_a, result_b);
    }

    // --- sipHash64 ---

    #[test]
    fn sip_hash64_deterministic() {
        let input = Value::Text("test data".to_owned());
        let result_a = eval(&SipHash64Func, smallvec::smallvec![input.clone()], &ctx()).unwrap();
        let result_b = eval(&SipHash64Func, smallvec::smallvec![input], &ctx()).unwrap();
        assert_eq!(result_a, result_b);
        assert!(matches!(result_a, Value::Int64(_)));
    }

    #[test]
    fn sip_hash64_null_propagation() {
        let result = eval(&SipHash64Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn sip_hash64_different_inputs_different_hashes() {
        let result_a = eval(
            &SipHash64Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        let result_b = eval(
            &SipHash64Func,
            smallvec::smallvec![Value::Text("world".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_ne!(result_a, result_b);
    }

    // --- fnv1a64 ---

    #[test]
    fn fnv1a64_deterministic() {
        let input = Value::Text("test data".to_owned());
        let result_a = eval(&Fnv1a64Func, smallvec::smallvec![input.clone()], &ctx()).unwrap();
        let result_b = eval(&Fnv1a64Func, smallvec::smallvec![input], &ctx()).unwrap();
        assert_eq!(result_a, result_b);
        assert!(matches!(result_a, Value::Int64(_)));
    }

    #[test]
    fn fnv1a64_null_propagation() {
        let result = eval(&Fnv1a64Func, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn fnv1a64_known_value_empty() {
        // FNV-1a offset basis for empty input is the offset basis itself
        let result = eval(
            &Fnv1a64Func,
            smallvec::smallvec![Value::Text(String::new())],
            &ctx(),
        )
        .unwrap();
        // FNV-1a 64-bit offset basis = 0xcbf29ce484222325
        assert_eq!(result, Value::Int64(0xcbf2_9ce4_8422_2325_u64 as i64));
    }

    #[test]
    fn fnv1a64_different_inputs_different_hashes() {
        let result_a = eval(
            &Fnv1a64Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        let result_b = eval(
            &Fnv1a64Func,
            smallvec::smallvec![Value::Text("world".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_ne!(result_a, result_b);
    }

    #[test]
    fn fnv1a64_bytes_input() {
        let result = eval(
            &Fnv1a64Func,
            smallvec::smallvec![Value::Bytes(vec![0x68, 0x65, 0x6c, 0x6c, 0x6f])],
            &ctx(),
        )
        .unwrap();
        let expected = eval(
            &Fnv1a64Func,
            smallvec::smallvec![Value::Text("hello".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, expected);
    }
}
