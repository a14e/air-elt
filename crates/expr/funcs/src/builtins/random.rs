use rand::RngExt;
use rand::distr::{Alphanumeric, SampleString};
use uuid::Uuid;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

/// Maximum length for random string generation (randomAlphanumeric, randomHex).
const MAX_STRING_LENGTH: i64 = 1024;

/// Maximum length for random bytes generation.
const MAX_BYTES_LENGTH: i64 = 1_048_576;

static RANDOM_UUID: RandomUuidFunc = RandomUuidFunc;
static RANDOM_INT: RandomIntFunc = RandomIntFunc;
static RANDOM_FLOAT: RandomFloatFunc = RandomFloatFunc;
static RANDOM_ALPHANUMERIC: RandomAlphanumericFunc = RandomAlphanumericFunc;
static RANDOM_HEX: RandomHexFunc = RandomHexFunc;
static RANDOM_BYTES: RandomBytesFunc = RandomBytesFunc;
static RANDOM_CHOICE: RandomChoiceFunc = RandomChoiceFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&RANDOM_UUID);
    registry.register(&RANDOM_INT);
    registry.register(&RANDOM_FLOAT);
    registry.register(&RANDOM_ALPHANUMERIC);
    registry.register(&RANDOM_HEX);
    registry.register(&RANDOM_BYTES);
    registry.register(&RANDOM_CHOICE);
}

// ---------------------------------------------------------------------------
// randomUuid
// ---------------------------------------------------------------------------

struct RandomUuidFunc;

impl ExprFunction for RandomUuidFunc {
    fn name(&self) -> &str {
        "randomUuid"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Uuid))
    }

    fn evaluate(&self, _args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let id = Uuid::new_v4();
        Ok(Value::Uuid(id))
    }
}

// ---------------------------------------------------------------------------
// randomInt
// ---------------------------------------------------------------------------

struct RandomIntFunc;

impl ExprFunction for RandomIntFunc {
    fn name(&self) -> &str {
        "randomInt"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Int64))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let max_val = args.remove(1);
        let min_val = args.remove(0);

        let min = extract_i64("randomInt", "min", &min_val)?;
        let max = extract_i64("randomInt", "max", &max_val)?;

        if min > max {
            return Err(FuncError::EvalFailed {
                function: "randomInt".to_owned(),
                reason: format!("min ({min}) must not exceed max ({max})"),
            });
        }

        let mut rng = rand::rng();
        let val = rng.random_range(min..=max);
        Ok(Value::Int64(val))
    }
}

// ---------------------------------------------------------------------------
// randomFloat
// ---------------------------------------------------------------------------

struct RandomFloatFunc;

impl ExprFunction for RandomFloatFunc {
    fn name(&self) -> &str {
        "randomFloat"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Float64))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let max_val = args.remove(1);
        let min_val = args.remove(0);

        let min = extract_f64("randomFloat", "min", &min_val)?;
        let max = extract_f64("randomFloat", "max", &max_val)?;

        if min > max {
            return Err(FuncError::EvalFailed {
                function: "randomFloat".to_owned(),
                reason: format!("min ({min}) must not exceed max ({max})"),
            });
        }

        let mut rng = rand::rng();
        let val: f64 = rng.random_range(min..max);
        Ok(Value::Float64(val))
    }
}

// ---------------------------------------------------------------------------
// randomAlphanumeric
// ---------------------------------------------------------------------------

struct RandomAlphanumericFunc;

impl ExprFunction for RandomAlphanumericFunc {
    fn name(&self) -> &str {
        "randomAlphanumeric"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let length_val = args.remove(0);
        let length = extract_length("randomAlphanumeric", &length_val, MAX_STRING_LENGTH)?;

        let mut rng = rand::rng();
        let s = Alphanumeric.sample_string(&mut rng, length as usize);
        Ok(Value::Text(s))
    }
}

// ---------------------------------------------------------------------------
// randomHex
// ---------------------------------------------------------------------------

struct RandomHexFunc;

impl ExprFunction for RandomHexFunc {
    fn name(&self) -> &str {
        "randomHex"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Text { size: None }))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let length_val = args.remove(0);
        let length = extract_length("randomHex", &length_val, MAX_STRING_LENGTH)?;

        let mut rng = rand::rng();
        let s: String = (0..length)
            .map(|_| {
                let nibble: u8 = rng.random_range(0..16);
                char::from(if nibble < 10 {
                    b'0' + nibble
                } else {
                    b'a' + nibble - 10
                })
            })
            .collect();
        Ok(Value::Text(s))
    }
}

// ---------------------------------------------------------------------------
// randomBytes
// ---------------------------------------------------------------------------

struct RandomBytesFunc;

impl ExprFunction for RandomBytesFunc {
    fn name(&self) -> &str {
        "randomBytes"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Bytes { size: None }))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let length_val = args.remove(0);
        let length = extract_length("randomBytes", &length_val, MAX_BYTES_LENGTH)?;

        let mut rng = rand::rng();
        let mut bytes = vec![0u8; length as usize];
        rng.fill(&mut bytes[..]);
        Ok(Value::Bytes(bytes))
    }
}

// ---------------------------------------------------------------------------
// randomChoice
// ---------------------------------------------------------------------------

struct RandomChoiceFunc;

impl ExprFunction for RandomChoiceFunc {
    fn name(&self) -> &str {
        "randomChoice"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        None
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        if args.is_empty() {
            return Err(FuncError::ArityMismatch {
                function: "randomChoice".to_owned(),
                expected: "at least 1".to_owned(),
                actual: 0,
            });
        }
        // The return type is nullable if any argument is nullable; the data type
        // is taken from the first argument (all args should be the same type in
        // practice, but we don't enforce that at type-check time).
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(args[0].data_type.clone(), nullable))
    }

    fn evaluate(&self, args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        if args.is_empty() {
            return Err(FuncError::ArityMismatch {
                function: "randomChoice".to_owned(),
                expected: "at least 1".to_owned(),
                actual: 0,
            });
        }
        let mut rng = rand::rng();
        let idx = rng.random_range(0..args.len());
        Ok(args.into_iter().nth(idx).expect("index within bounds"))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_i64(function: &str, param_name: &str, value: &Value) -> Result<i64, FuncError> {
    match value {
        Value::Int64(n) => Ok(*n),
        other => Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: format!("Int64 for {param_name}"),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_f64(function: &str, param_name: &str, value: &Value) -> Result<f64, FuncError> {
    match value {
        Value::Float64(n) => Ok(*n),
        Value::Int64(n) => Ok(*n as f64),
        other => Err(FuncError::TypeMismatch {
            function: function.to_owned(),
            expected: format!("Float64 or Int64 for {param_name}"),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_length(function: &str, value: &Value, max: i64) -> Result<i64, FuncError> {
    let n = match value {
        Value::Int64(n) => *n,
        other => {
            return Err(FuncError::TypeMismatch {
                function: function.to_owned(),
                expected: "Int64 for length".to_owned(),
                actual: format!("{:?}", other.data_type()),
            });
        }
    };
    if n < 0 {
        return Err(FuncError::EvalFailed {
            function: function.to_owned(),
            reason: format!("length must be non-negative, got {n}"),
        });
    }
    if n > max {
        return Err(FuncError::EvalFailed {
            function: function.to_owned(),
            reason: format!("length {n} exceeds maximum {max}"),
        });
    }
    Ok(n)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use super::*;
    use crate::signature::EvalContext;

    fn ctx() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(crate::test_support::EmptyEnv),
            file_resolver: Arc::new(crate::test_support::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn random_uuid_produces_valid_v4() {
        let f = RandomUuidFunc;
        let result = f.evaluate(vec![], &ctx()).unwrap();
        match result {
            Value::Uuid(id) => {
                assert_eq!(id.get_version_num(), 4);
            }
            other => panic!("expected Uuid, got {other:?}"),
        }
    }

    #[test]
    fn random_uuid_produces_different_values() {
        let f = RandomUuidFunc;
        let r1 = f.evaluate(vec![], &ctx()).unwrap();
        let r2 = f.evaluate(vec![], &ctx()).unwrap();
        assert_ne!(r1, r2);
    }

    #[test]
    fn random_int_in_range() {
        let f = RandomIntFunc;
        for _ in 0..100 {
            let result = f
                .evaluate(vec![Value::Int64(10), Value::Int64(20)], &ctx())
                .unwrap();
            match result {
                Value::Int64(n) => {
                    assert!((10..=20).contains(&n), "got {n} outside [10, 20]");
                }
                other => panic!("expected Int64, got {other:?}"),
            }
        }
    }

    #[test]
    fn random_int_equal_bounds() {
        let f = RandomIntFunc;
        let result = f
            .evaluate(vec![Value::Int64(5), Value::Int64(5)], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(5));
    }

    #[test]
    fn random_int_min_exceeds_max() {
        let f = RandomIntFunc;
        let result = f.evaluate(vec![Value::Int64(20), Value::Int64(10)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn random_float_in_range() {
        let f = RandomFloatFunc;
        for _ in 0..100 {
            let result = f
                .evaluate(vec![Value::Float64(1.0), Value::Float64(2.0)], &ctx())
                .unwrap();
            match result {
                Value::Float64(n) => {
                    assert!((1.0..2.0).contains(&n), "got {n} outside [1.0, 2.0)");
                }
                other => panic!("expected Float64, got {other:?}"),
            }
        }
    }

    #[test]
    fn random_alphanumeric_correct_length() {
        let f = RandomAlphanumericFunc;
        let result = f.evaluate(vec![Value::Int64(32)], &ctx()).unwrap();
        match result {
            Value::Text(s) => {
                assert_eq!(s.len(), 32);
                assert!(s.chars().all(|c| c.is_ascii_alphanumeric()));
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn random_alphanumeric_zero_length() {
        let f = RandomAlphanumericFunc;
        let result = f.evaluate(vec![Value::Int64(0)], &ctx()).unwrap();
        assert_eq!(result, Value::Text(String::new()));
    }

    #[test]
    fn random_alphanumeric_exceeds_max() {
        let f = RandomAlphanumericFunc;
        let result = f.evaluate(vec![Value::Int64(2000)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn random_hex_correct_length_and_chars() {
        let f = RandomHexFunc;
        let result = f.evaluate(vec![Value::Int64(16)], &ctx()).unwrap();
        match result {
            Value::Text(s) => {
                assert_eq!(s.len(), 16);
                assert!(
                    s.chars()
                        .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase())
                );
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn random_bytes_correct_length() {
        let f = RandomBytesFunc;
        let result = f.evaluate(vec![Value::Int64(64)], &ctx()).unwrap();
        match result {
            Value::Bytes(b) => assert_eq!(b.len(), 64),
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn random_bytes_exceeds_max() {
        let f = RandomBytesFunc;
        let result = f.evaluate(vec![Value::Int64(2_000_000)], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn random_choice_returns_one_of_args() {
        let f = RandomChoiceFunc;
        let options = vec![
            Value::Text("a".into()),
            Value::Text("b".into()),
            Value::Text("c".into()),
        ];
        for _ in 0..50 {
            let result = f.evaluate(options.clone(), &ctx()).unwrap();
            match &result {
                Value::Text(s) => {
                    assert!(s == "a" || s == "b" || s == "c", "unexpected value: {s}");
                }
                other => panic!("expected Text, got {other:?}"),
            }
        }
    }

    #[test]
    fn random_choice_single_arg() {
        let f = RandomChoiceFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(42));
    }
}
