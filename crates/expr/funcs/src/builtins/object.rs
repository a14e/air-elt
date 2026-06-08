use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{ArgWindow, EvalContext, ExprFunction};

static OBJECT_LENGTH: ObjectLengthFunc = ObjectLengthFunc;
static OBJECT_KEYS: ObjectKeysFunc = ObjectKeysFunc;
static OBJECT_VALUES: ObjectValuesFunc = ObjectValuesFunc;
static OBJECT_HAS_KEY: ObjectHasKeyFunc = ObjectHasKeyFunc;
static OBJECT_GET: ObjectGetFunc = ObjectGetFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&OBJECT_LENGTH);
    registry.register(&OBJECT_KEYS);
    registry.register(&OBJECT_VALUES);
    registry.register(&OBJECT_HAS_KEY);
    registry.register(&OBJECT_GET);
}

fn extract_object_ref<'a>(
    val: &'a Value,
    func_name: &str,
) -> Result<&'a [(String, Value)], FuncError> {
    match val {
        Value::Object(entries) => Ok(entries),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Object".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_int64_ref(val: &Value, func_name: &str) -> Result<i64, FuncError> {
    match val {
        Value::Int64(n) => Ok(*n),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Int64".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

fn extract_text_ref<'a>(val: &'a Value, func_name: &str) -> Result<&'a str, FuncError> {
    match val {
        Value::Text(s) => Ok(s),
        other => Err(FuncError::TypeMismatch {
            function: func_name.to_owned(),
            expected: "Text".to_owned(),
            actual: format!("{:?}", other.data_type()),
        }),
    }
}

struct ObjectLengthFunc;

impl ExprFunction for ObjectLengthFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "objectLength"
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
        let val = args.read(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let entries = extract_object_ref(val, "objectLength")?;
        Ok(Value::Int64(entries.len() as i64))
    }
}

struct ObjectKeysFunc;

impl ExprFunction for ObjectKeysFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "objectKeys"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(DataType::Text { size: None }))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let idx_val = args.read(1);
        let obj_val = args.read(0);
        if obj_val.is_null() || idx_val.is_null() {
            return Ok(Value::Null);
        }
        let entries = extract_object_ref(obj_val, "objectKeys")?;
        let idx = extract_int64_ref(idx_val, "objectKeys")?;
        if idx < 0 {
            return Ok(Value::Null);
        }
        let idx = idx as usize;
        match entries.get(idx) {
            Some((key, _)) => Ok(Value::Text(key.clone())),
            None => Ok(Value::Null),
        }
    }
}

struct ObjectValuesFunc;

impl ExprFunction for ObjectValuesFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "objectValues"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(DataType::Json))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let idx_val = args.read(1);
        let obj_val = args.read(0);
        if obj_val.is_null() || idx_val.is_null() {
            return Ok(Value::Null);
        }
        let entries = extract_object_ref(obj_val, "objectValues")?;
        let idx = extract_int64_ref(idx_val, "objectValues")?;
        if idx < 0 {
            return Ok(Value::Null);
        }
        let idx = idx as usize;
        match entries.get(idx) {
            Some((_, value)) => Ok(value.clone()),
            None => Ok(Value::Null),
        }
    }
}

struct ObjectHasKeyFunc;

impl ExprFunction for ObjectHasKeyFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "objectHasKey"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        let nullable = args.iter().any(|a| a.nullable);
        Ok(NullableExprType::new(DataType::Bool, nullable))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let key_val = args.read(1);
        let obj_val = args.read(0);
        if obj_val.is_null() || key_val.is_null() {
            return Ok(Value::Null);
        }
        let entries = extract_object_ref(obj_val, "objectHasKey")?;
        let key = extract_text_ref(key_val, "objectHasKey")?;
        let has = entries.iter().any(|(k, _)| k == key);
        Ok(Value::Bool(has))
    }
}

struct ObjectGetFunc;

impl ExprFunction for ObjectGetFunc {
    fn is_pure(&self) -> bool {
        true
    }

    fn name(&self) -> &str {
        "objectGet"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(DataType::Json))
    }

    fn evaluate(
        &self,
        args: &mut dyn ArgWindow,
        _context: &EvalContext,
    ) -> Result<Value, FuncError> {
        let key_val = args.read(1);
        let obj_val = args.read(0);
        if obj_val.is_null() || key_val.is_null() {
            return Ok(Value::Null);
        }
        let entries = extract_object_ref(obj_val, "objectGet")?;
        let key = extract_text_ref(key_val, "objectGet")?;
        match entries.iter().find(|(k, _)| k == key) {
            Some((_, value)) => Ok(value.clone()),
            None => Ok(Value::Null),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::{ctx, eval};

    fn sample_object() -> Value {
        Value::Object(vec![
            ("name".to_owned(), Value::Text("alice".to_owned())),
            ("age".to_owned(), Value::Int64(30)),
            ("active".to_owned(), Value::Bool(true)),
        ])
    }

    #[test]
    fn object_length_basic() {
        let f = ObjectLengthFunc;
        let result = eval(&f, smallvec::smallvec![sample_object()], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn object_length_null_propagation() {
        let f = ObjectLengthFunc;
        let result = eval(&f, smallvec::smallvec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn object_keys_valid_index() {
        let f = ObjectKeysFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Int64(0)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("name".to_owned()));

        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Int64(1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Text("age".to_owned()));
    }

    #[test]
    fn object_keys_out_of_bounds() {
        let f = ObjectKeysFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Int64(10)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn object_keys_negative_index() {
        let f = ObjectKeysFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Int64(-1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn object_values_valid_index() {
        let f = ObjectValuesFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Int64(1)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(30));
    }

    #[test]
    fn object_values_out_of_bounds() {
        let f = ObjectValuesFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Int64(10)],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn object_has_key_present() {
        let f = ObjectHasKeyFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Text("name".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn object_has_key_absent() {
        let f = ObjectHasKeyFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Text("missing".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn object_has_key_null_propagation() {
        let f = ObjectHasKeyFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Null, Value::Text("key".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);

        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Null],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn object_get_present() {
        let f = ObjectGetFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Text("age".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Int64(30));
    }

    #[test]
    fn object_get_absent() {
        let f = ObjectGetFunc;
        let result = eval(
            &f,
            smallvec::smallvec![sample_object(), Value::Text("missing".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn object_get_null_propagation() {
        let f = ObjectGetFunc;
        let result = eval(
            &f,
            smallvec::smallvec![Value::Null, Value::Text("key".to_owned())],
            &ctx(),
        )
        .unwrap();
        assert_eq!(result, Value::Null);
    }
}
