use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};
use jsonpath_rust::JsonPath;
use std::str::FromStr;

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static PARSE_JSON: ParseJsonFunc = ParseJsonFunc;
static TO_JSON: ToJsonFunc = ToJsonFunc;
static JS_PATH: JsPathFunc = JsPathFunc;
static JS_PATH_STRING: JsPathStringFunc = JsPathStringFunc;
static JS_PATH_INT: JsPathIntFunc = JsPathIntFunc;
static JS_PATH_FLOAT: JsPathFloatFunc = JsPathFloatFunc;
static JS_PATH_BOOL: JsPathBoolFunc = JsPathBoolFunc;
static JSON_LENGTH: JsonLengthFunc = JsonLengthFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&PARSE_JSON);
    registry.register(&TO_JSON);
    registry.register(&JS_PATH);
    registry.register(&JS_PATH_STRING);
    registry.register(&JS_PATH_INT);
    registry.register(&JS_PATH_FLOAT);
    registry.register(&JS_PATH_BOOL);
    registry.register(&JSON_LENGTH);
}

/// Extracts the first result from a JSONPath query, returning None if empty.
fn extract_first_json_path(
    json: &serde_json::Value,
    path_str: &str,
) -> Result<Option<serde_json::Value>, FuncError> {
    let path = JsonPath::from_str(path_str).map_err(|e| FuncError::JsonPathError {
        reason: e.to_string(),
    })?;
    let results = path.find(json);
    match results {
        serde_json::Value::Array(arr) => Ok(arr.into_iter().next()),
        serde_json::Value::Null => Ok(None),
        other => Ok(Some(other)),
    }
}

/// Converts a Value to serde_json::Value for JSONPath operations.
fn value_to_serde_json(val: &Value) -> Result<serde_json::Value, FuncError> {
    match val {
        Value::Json(j) => Ok(j.clone()),
        Value::Object(entries) => {
            let map: serde_json::Map<String, serde_json::Value> = entries
                .iter()
                .map(|(k, v)| {
                    let json_v = air_elt_types::value_to_json(v).unwrap_or(serde_json::Value::Null);
                    (k.clone(), json_v)
                })
                .collect();
            Ok(serde_json::Value::Object(map))
        }
        other => air_elt_types::value_to_json(other).map_err(|e| FuncError::EvalFailed {
            function: "toJson".to_owned(),
            reason: e.to_string(),
        }),
    }
}

struct ParseJsonFunc;

impl ExprFunction for ParseJsonFunc {
    fn name(&self) -> &str {
        "parseJson"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Json, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let s = match val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "parseJson".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let parsed: serde_json::Value =
            serde_json::from_str(&s).map_err(|e| FuncError::EvalFailed {
                function: "parseJson".to_owned(),
                reason: e.to_string(),
            })?;
        Ok(Value::Json(parsed))
    }
}

struct ToJsonFunc;

impl ExprFunction for ToJsonFunc {
    fn name(&self) -> &str {
        "toJson"
    }

    fn min_args(&self) -> usize {
        1
    }

    fn max_args(&self) -> Option<usize> {
        Some(1)
    }

    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::new(DataType::Json, args[0].nullable))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        match val {
            Value::Json(j) => Ok(Value::Json(j)),
            other => {
                let j = value_to_serde_json(&other)?;
                Ok(Value::Json(j))
            }
        }
    }
}

struct JsPathFunc;

impl ExprFunction for JsPathFunc {
    fn name(&self) -> &str {
        "jsPath"
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let path_val = args.remove(1);
        let json_val = args.remove(0);
        if json_val.is_null() || path_val.is_null() {
            return Ok(Value::Null);
        }
        let json = match json_val {
            Value::Json(j) => j,
            other => value_to_serde_json(&other)?,
        };
        let path_str = match path_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "jsPath".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match extract_first_json_path(&json, &path_str)? {
            Some(v) => Ok(Value::Json(v)),
            None => Ok(Value::Null),
        }
    }
}

struct JsPathStringFunc;

impl ExprFunction for JsPathStringFunc {
    fn name(&self) -> &str {
        "jsPathString"
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let path_val = args.remove(1);
        let json_val = args.remove(0);
        if json_val.is_null() || path_val.is_null() {
            return Ok(Value::Null);
        }
        let json = match json_val {
            Value::Json(j) => j,
            other => value_to_serde_json(&other)?,
        };
        let path_str = match path_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "jsPathString".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match extract_first_json_path(&json, &path_str)? {
            Some(serde_json::Value::String(s)) => Ok(Value::Text(s)),
            Some(_) | None => Ok(Value::Null),
        }
    }
}

struct JsPathIntFunc;

impl ExprFunction for JsPathIntFunc {
    fn name(&self) -> &str {
        "jsPathInt"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(DataType::Int64))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let path_val = args.remove(1);
        let json_val = args.remove(0);
        if json_val.is_null() || path_val.is_null() {
            return Ok(Value::Null);
        }
        let json = match json_val {
            Value::Json(j) => j,
            other => value_to_serde_json(&other)?,
        };
        let path_str = match path_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "jsPathInt".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match extract_first_json_path(&json, &path_str)? {
            Some(serde_json::Value::Number(n)) => match n.as_i64() {
                Some(i) => Ok(Value::Int64(i)),
                None => Ok(Value::Null),
            },
            Some(_) | None => Ok(Value::Null),
        }
    }
}

struct JsPathFloatFunc;

impl ExprFunction for JsPathFloatFunc {
    fn name(&self) -> &str {
        "jsPathFloat"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(DataType::Float64))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let path_val = args.remove(1);
        let json_val = args.remove(0);
        if json_val.is_null() || path_val.is_null() {
            return Ok(Value::Null);
        }
        let json = match json_val {
            Value::Json(j) => j,
            other => value_to_serde_json(&other)?,
        };
        let path_str = match path_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "jsPathFloat".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match extract_first_json_path(&json, &path_str)? {
            Some(serde_json::Value::Number(n)) => match n.as_f64() {
                Some(f) => Ok(Value::Float64(f)),
                None => Ok(Value::Null),
            },
            Some(_) | None => Ok(Value::Null),
        }
    }
}

struct JsPathBoolFunc;

impl ExprFunction for JsPathBoolFunc {
    fn name(&self) -> &str {
        "jsPathBool"
    }

    fn min_args(&self) -> usize {
        2
    }

    fn max_args(&self) -> Option<usize> {
        Some(2)
    }

    fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
        Ok(NullableExprType::nullable(DataType::Bool))
    }

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let path_val = args.remove(1);
        let json_val = args.remove(0);
        if json_val.is_null() || path_val.is_null() {
            return Ok(Value::Null);
        }
        let json = match json_val {
            Value::Json(j) => j,
            other => value_to_serde_json(&other)?,
        };
        let path_str = match path_val {
            Value::Text(s) => s,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "jsPathBool".to_owned(),
                    expected: "Text".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        match extract_first_json_path(&json, &path_str)? {
            Some(serde_json::Value::Bool(b)) => Ok(Value::Bool(b)),
            Some(_) | None => Ok(Value::Null),
        }
    }
}

struct JsonLengthFunc;

impl ExprFunction for JsonLengthFunc {
    fn name(&self) -> &str {
        "jsonLength"
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

    fn evaluate(&self, mut args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
        let val = args.remove(0);
        if val.is_null() {
            return Ok(Value::Null);
        }
        let json = match val {
            Value::Json(j) => j,
            other => {
                return Err(FuncError::TypeMismatch {
                    function: "jsonLength".to_owned(),
                    expected: "Json".to_owned(),
                    actual: format!("{:?}", other.data_type()),
                });
            }
        };
        let len = match &json {
            serde_json::Value::Array(arr) => arr.len() as i64,
            serde_json::Value::Object(map) => map.len() as i64,
            _ => {
                return Err(FuncError::EvalFailed {
                    function: "jsonLength".to_owned(),
                    reason: "expected array or object".to_owned(),
                });
            }
        };
        Ok(Value::Int64(len))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    #[test]
    fn parse_json_valid() {
        let f = ParseJsonFunc;
        let result = f
            .evaluate(
                vec![Value::Text(r#"{"name":"alice","age":30}"#.to_owned())],
                &ctx(),
            )
            .unwrap();
        match result {
            Value::Json(j) => {
                assert_eq!(j["name"], serde_json::Value::String("alice".to_owned()));
                assert_eq!(j["age"], serde_json::json!(30));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn parse_json_invalid() {
        let f = ParseJsonFunc;
        let result = f.evaluate(vec![Value::Text("not json{".to_owned())], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn parse_json_null_propagation() {
        let f = ParseJsonFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn to_json_roundtrip() {
        let f = ToJsonFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Json(serde_json::json!(42)));

        let result = f
            .evaluate(vec![Value::Text("hello".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Json(serde_json::json!("hello")));
    }

    #[test]
    fn to_json_passthrough() {
        let f = ToJsonFunc;
        let input = serde_json::json!({"a": 1});
        let result = f
            .evaluate(vec![Value::Json(input.clone())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Json(input));
    }

    #[test]
    fn js_path_extraction() {
        let f = JsPathFunc;
        let json = Value::Json(serde_json::json!({"store": {"book": [{"title": "Rust"}]}}));
        let result = f
            .evaluate(
                vec![json, Value::Text("$.store.book[0].title".to_owned())],
                &ctx(),
            )
            .unwrap();
        assert_eq!(result, Value::Json(serde_json::json!("Rust")));
    }

    #[test]
    fn js_path_not_found() {
        let f = JsPathFunc;
        let json = Value::Json(serde_json::json!({"a": 1}));
        let result = f
            .evaluate(vec![json, Value::Text("$.nonexistent".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn js_path_string_extraction() {
        let f = JsPathStringFunc;
        let json = Value::Json(serde_json::json!({"name": "alice"}));
        let result = f
            .evaluate(vec![json, Value::Text("$.name".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Text("alice".to_owned()));
    }

    #[test]
    fn js_path_int_extraction() {
        let f = JsPathIntFunc;
        let json = Value::Json(serde_json::json!({"count": 42}));
        let result = f
            .evaluate(vec![json, Value::Text("$.count".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn js_path_float_extraction() {
        let f = JsPathFloatFunc;
        let json = Value::Json(serde_json::json!({"rate": 9.75}));
        let result = f
            .evaluate(vec![json, Value::Text("$.rate".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Float64(9.75));
    }

    #[test]
    fn js_path_bool_extraction() {
        let f = JsPathBoolFunc;
        let json = Value::Json(serde_json::json!({"active": true}));
        let result = f
            .evaluate(vec![json, Value::Text("$.active".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn js_path_type_mismatch_returns_null() {
        let f = JsPathIntFunc;
        let json = Value::Json(serde_json::json!({"name": "alice"}));
        let result = f
            .evaluate(vec![json, Value::Text("$.name".to_owned())], &ctx())
            .unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn json_length_array() {
        let f = JsonLengthFunc;
        let json = Value::Json(serde_json::json!([1, 2, 3]));
        let result = f.evaluate(vec![json], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn json_length_object() {
        let f = JsonLengthFunc;
        let json = Value::Json(serde_json::json!({"a": 1, "b": 2}));
        let result = f.evaluate(vec![json], &ctx()).unwrap();
        assert_eq!(result, Value::Int64(2));
    }

    #[test]
    fn json_length_scalar_error() {
        let f = JsonLengthFunc;
        let json = Value::Json(serde_json::json!(42));
        let result = f.evaluate(vec![json], &ctx());
        assert!(result.is_err());
    }

    #[test]
    fn json_length_null_propagation() {
        let f = JsonLengthFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Null);
    }
}
