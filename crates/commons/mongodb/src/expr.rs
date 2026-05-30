//! MongoDB-specific expression functions.
//!
//! Provides `objectId(hex_string)`, `seconds(objectId_bytes)`, and
//! `newObjectId()` for use in the expression language.

use air_elt_expr_funcs::error::FuncError;
use air_elt_expr_funcs::registry::FunctionRegistry;
use air_elt_expr_funcs::signature::{EvalContext, ExprFunction};
use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use rand::Rng;

// ---------------------------------------------------------------------------
// objectId(hex_string) -> Bytes(12)
// ---------------------------------------------------------------------------

struct ObjectIdFunc;

impl ExprFunction for ObjectIdFunc {
    fn name(&self) -> &str {
        "objectId"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args[0].nullable;
        Ok(NullableExprType::new(
            DataType::Bytes { size: Some(12) },
            nullable,
        ))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let hex_str = match args.remove(0) {
            Value::Text(s) => s,
            Value::Null => return Ok(Value::Null),
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "objectId".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        if hex_str.len() != 24 {
            return Err(FuncError::EvalFailed {
                function: "objectId".to_owned(),
                reason: format!("ObjectId hex must be 24 characters, got {}", hex_str.len()),
            });
        }
        let bytes = hex::decode(&hex_str).map_err(|e| FuncError::EvalFailed {
            function: "objectId".to_owned(),
            reason: format!("invalid hex: {e}"),
        })?;
        Ok(Value::Bytes(bytes))
    }
}

// ---------------------------------------------------------------------------
// seconds(objectId_bytes) -> Int64
// ---------------------------------------------------------------------------

struct SecondsFunc;

impl ExprFunction for SecondsFunc {
    fn name(&self) -> &str {
        "seconds"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args[0].nullable;
        Ok(NullableExprType::new(DataType::Int64, nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let bytes = match args.remove(0) {
            Value::Bytes(b) => b,
            Value::Null => return Ok(Value::Null),
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "seconds".to_owned(),
                    expected: "Bytes".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        if bytes.len() < 4 {
            return Err(FuncError::EvalFailed {
                function: "seconds".to_owned(),
                reason: "input must be at least 4 bytes (ObjectId)".to_owned(),
            });
        }
        let timestamp = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        Ok(Value::Int64(i64::from(timestamp)))
    }
}

// ---------------------------------------------------------------------------
// newObjectId() -> Bytes(12)
// ---------------------------------------------------------------------------

struct NewObjectIdFunc;

impl ExprFunction for NewObjectIdFunc {
    fn name(&self) -> &str {
        "newObjectId"
    }

    fn min_args(&self) -> usize {
        0
    }

    fn max_args(&self) -> Option<usize> {
        Some(0)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::non_null(DataType::Bytes {
            size: Some(12),
        }))
    }

    fn evaluate(&self, _args: Vec<Value>, context: &EvalContext) -> Result<Value, FuncError> {
        let timestamp = context.now.timestamp() as u32;
        let mut bytes = [0u8; 12];
        bytes[0..4].copy_from_slice(&timestamp.to_be_bytes());
        rand::rng().fill_bytes(&mut bytes[4..]);
        Ok(Value::Bytes(bytes.to_vec()))
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

static OBJECT_ID: ObjectIdFunc = ObjectIdFunc;
static SECONDS: SecondsFunc = SecondsFunc;
static NEW_OBJECT_ID: NewObjectIdFunc = NewObjectIdFunc;

/// Register MongoDB-specific expression functions into the given registry.
pub fn register_functions(registry: &mut FunctionRegistry) {
    registry.register(&OBJECT_ID);
    registry.register(&SECONDS);
    registry.register(&NEW_OBJECT_ID);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    use std::path::PathBuf;
    use std::sync::Arc;

    use chrono::Utc;

    struct EmptyEnv;

    impl air_elt_expr_funcs::signature::EnvResolver for EmptyEnv {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
    }

    struct NoopFiles;

    impl air_elt_expr_funcs::signature::FileResolver for NoopFiles {
        fn read(&self, path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
            Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: "not implemented".to_owned(),
            })
        }
    }

    fn test_context() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(EmptyEnv),
            file_resolver: Arc::new(NoopFiles),
            now: Utc::now(),
            base_dir: PathBuf::from("."),
            is_compile_time: false,
            caches: air_elt_expr_funcs::ExprCaches::default(),
        }
    }

    #[test]
    fn object_id_valid_hex() {
        let ctx = test_context();
        let args = vec![Value::Text("507f1f77bcf86cd799439011".to_owned())];
        let result = OBJECT_ID.evaluate(args, &ctx).unwrap();
        let expected = hex::decode("507f1f77bcf86cd799439011").unwrap();
        assert_eq!(result, Value::Bytes(expected));
    }

    #[test]
    fn object_id_wrong_length() {
        let ctx = test_context();
        let args = vec![Value::Text("abcdef".to_owned())];
        let result = OBJECT_ID.evaluate(args, &ctx);
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn object_id_invalid_hex() {
        let ctx = test_context();
        let args = vec![Value::Text("zzzzzzzzzzzzzzzzzzzzzzzz".to_owned())];
        let result = OBJECT_ID.evaluate(args, &ctx);
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn object_id_null_propagation() {
        let ctx = test_context();
        let args = vec![Value::Null];
        let result = OBJECT_ID.evaluate(args, &ctx).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn seconds_extracts_timestamp() {
        let ctx = test_context();
        // ObjectId "507f1f77..." has first 4 bytes = 0x507f1f77 = 1350508407
        let oid_bytes = hex::decode("507f1f77bcf86cd799439011").unwrap();
        let args = vec![Value::Bytes(oid_bytes)];
        let result = SECONDS.evaluate(args, &ctx).unwrap();
        assert_eq!(result, Value::Int64(1_350_508_407));
    }

    #[test]
    fn seconds_too_short() {
        let ctx = test_context();
        let args = vec![Value::Bytes(vec![1, 2, 3])];
        let result = SECONDS.evaluate(args, &ctx);
        assert!(matches!(result, Err(FuncError::EvalFailed { .. })));
    }

    #[test]
    fn seconds_null_propagation() {
        let ctx = test_context();
        let args = vec![Value::Null];
        let result = SECONDS.evaluate(args, &ctx).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn new_object_id_produces_12_bytes() {
        let ctx = test_context();
        let result = NEW_OBJECT_ID.evaluate(vec![], &ctx).unwrap();
        match result {
            Value::Bytes(b) => assert_eq!(b.len(), 12),
            other => panic!("expected Bytes, got {other:?}"),
        }
    }

    #[test]
    fn new_object_id_embeds_timestamp() {
        let ctx = test_context();
        let expected_ts = ctx.now.timestamp() as u32;
        let result = NEW_OBJECT_ID.evaluate(vec![], &ctx).unwrap();
        match result {
            Value::Bytes(b) => {
                let ts = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
                assert_eq!(ts, expected_ts);
            }
            other => panic!("expected Bytes, got {other:?}"),
        }
    }
}
