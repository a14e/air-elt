use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::{DataType, Value};

use crate::error::FuncError;
use crate::registry::FunctionRegistry;
use crate::signature::{EvalContext, ExprFunction};

static TYPE_OF: TypeOfFunc = TypeOfFunc;

pub fn register(registry: &mut FunctionRegistry) {
    registry.register(&TYPE_OF);
}

struct TypeOfFunc;

impl ExprFunction for TypeOfFunc {
    fn name(&self) -> &str {
        "typeof"
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
        let a = args.remove(0);
        let type_name = match &a {
            Value::Null => "Null".to_owned(),
            Value::Bool(_) => "Bool".to_owned(),
            Value::Int8(_) => "Int8".to_owned(),
            Value::Int16(_) => "Int16".to_owned(),
            Value::Int32(_) => "Int32".to_owned(),
            Value::Int64(_) => "Int64".to_owned(),
            Value::UInt8(_) => "UInt8".to_owned(),
            Value::UInt16(_) => "UInt16".to_owned(),
            Value::UInt32(_) => "UInt32".to_owned(),
            Value::UInt64(_) => "UInt64".to_owned(),
            Value::Float32(_) => "Float32".to_owned(),
            Value::Float64(_) => "Float64".to_owned(),
            Value::BigInt(_) => "BigInt".to_owned(),
            Value::Decimal(_) => "Decimal".to_owned(),
            Value::Text(_) => "Text".to_owned(),
            Value::Bytes(_) => "Bytes".to_owned(),
            Value::Date(_) => "Date".to_owned(),
            Value::Timestamp(_) => "Timestamp".to_owned(),
            Value::Uuid(_) => "Uuid".to_owned(),
            Value::Ipv4(_) => "Ipv4".to_owned(),
            Value::Ipv6(_) => "Ipv6".to_owned(),
            Value::Json(_) => "Json".to_owned(),
            Value::Object(_) => "Object".to_owned(),
            Value::Custom(v) => v.dyn_type().kind().to_owned(),
        };
        Ok(Value::Text(type_name))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::signature::EvalContext;
    use std::path::PathBuf;

    fn ctx() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(crate::tests::EmptyEnv),
            file_resolver: Arc::new(crate::tests::NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::new(),
        }
    }

    #[test]
    fn typeof_int64() {
        let f = TypeOfFunc;
        let result = f.evaluate(vec![Value::Int64(42)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Int64".into()));
    }

    #[test]
    fn typeof_text() {
        let f = TypeOfFunc;
        let result = f.evaluate(vec![Value::Text("hi".into())], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Text".into()));
    }

    #[test]
    fn typeof_null() {
        let f = TypeOfFunc;
        let result = f.evaluate(vec![Value::Null], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Null".into()));
    }

    #[test]
    fn typeof_bool() {
        let f = TypeOfFunc;
        let result = f.evaluate(vec![Value::Bool(true)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Bool".into()));
    }

    #[test]
    fn typeof_float64() {
        let f = TypeOfFunc;
        let result = f.evaluate(vec![Value::Float64(1.5)], &ctx()).unwrap();
        assert_eq!(result, Value::Text("Float64".into()));
    }
}
