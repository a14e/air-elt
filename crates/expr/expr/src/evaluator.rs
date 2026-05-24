use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_types::limits::MAX_EXPR_DEPTH;
use air_elt_types::Value;

use crate::ast::{Expr, InterpolationSegment, LiteralValue, Program, Statement};
use crate::error::ExprError;

/// Evaluate a parsed program with a function registry and evaluation context.
pub fn evaluate(
    program: &Program,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Value, ExprError> {
    let mut evaluator = Evaluator::new(registry, context);
    evaluator.eval_program(program)
}

/// Parse and evaluate an expression string in one call.
pub fn eval_expression(
    input: &str,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Value, ExprError> {
    let program = crate::parser::parse(input)?;
    evaluate(&program, registry, context)
}

/// Evaluate a string with interpolation segments.
/// Scans for unescaped `{expr}` markers, parses each inner expression,
/// evaluates it, and concatenates the results into a single string.
pub fn eval_interpolated(
    input: &str,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<String, ExprError> {
    let mut result = String::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'{' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'{' {
                result.push('{');
                i += 2;
                continue;
            }

            let start = i + 1;
            let mut depth = 1u32;
            let mut j = start;
            while j < bytes.len() && depth > 0 {
                if bytes[j] == b'{' {
                    depth += 1;
                } else if bytes[j] == b'}' {
                    depth -= 1;
                }
                if depth > 0 {
                    j += 1;
                }
            }

            if depth != 0 {
                return Err(ExprError::UnterminatedInterpolation { position: i });
            }

            let expr_source = &input[start..j];
            let value = eval_expression(expr_source, registry, context)?;
            result.push_str(&value_to_string(&value));
            i = j + 1;
        } else if bytes[i] == b'}' && i + 1 < bytes.len() && bytes[i + 1] == b'}' {
            result.push('}');
            i += 2;
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    Ok(result)
}

struct Evaluator<'a> {
    registry: &'a FunctionRegistry,
    context: &'a EvalContext,
    variables: AHashMap<String, Value>,
    depth: usize,
}

impl<'a> Evaluator<'a> {
    fn new(registry: &'a FunctionRegistry, context: &'a EvalContext) -> Self {
        Self {
            registry,
            context,
            variables: AHashMap::new(),
            depth: 0,
        }
    }

    fn eval_program(&mut self, program: &Program) -> Result<Value, ExprError> {
        for statement in &program.statements {
            self.eval_statement(statement)?;
        }
        self.eval_expr(&program.result)
    }

    fn eval_statement(&mut self, statement: &Statement) -> Result<(), ExprError> {
        let value = self.eval_expr(&statement.value)?;
        self.variables.insert(statement.name.clone(), value);
        Ok(())
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, ExprError> {
        match expr {
            Expr::Literal(lit) => Ok(eval_literal(lit)),
            Expr::Variable(name) => self.eval_variable(name),
            Expr::FunctionCall { name, args } => self.eval_function_call(name, args),
            Expr::Interpolation(segments) => self.eval_interpolation(segments),
            Expr::Object(entries) => self.eval_object(entries),
        }
    }

    fn eval_variable(&self, name: &str) -> Result<Value, ExprError> {
        self.variables
            .get(name)
            .cloned()
            .ok_or_else(|| ExprError::UndefinedVariable {
                name: name.to_string(),
            })
    }

    fn eval_function_call(&mut self, name: &str, args: &[Expr]) -> Result<Value, ExprError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            self.depth -= 1;
            return Err(ExprError::NestingTooDeep {
                max: MAX_EXPR_DEPTH,
            });
        }

        let function = self.registry.resolve(name, args.len())?;

        let mut evaluated_args = Vec::with_capacity(args.len());
        for arg in args {
            evaluated_args.push(self.eval_expr(arg)?);
        }

        let result = function.evaluate(evaluated_args, self.context)?;
        self.depth -= 1;
        Ok(result)
    }

    fn eval_interpolation(
        &mut self,
        segments: &[InterpolationSegment],
    ) -> Result<Value, ExprError> {
        let mut result = String::new();
        for segment in segments {
            match segment {
                InterpolationSegment::Text(s) => result.push_str(s),
                InterpolationSegment::Expression(expr) => {
                    let value = self.eval_expr(expr)?;
                    result.push_str(&value_to_string(&value));
                }
            }
        }
        Ok(Value::Text(result))
    }

    fn eval_object(&mut self, entries: &[(String, Expr)]) -> Result<Value, ExprError> {
        let mut map = serde_json::Map::with_capacity(entries.len());
        for (key, value_expr) in entries {
            let value = self.eval_expr(value_expr)?;
            map.insert(key.clone(), value_to_json(&value));
        }
        Ok(Value::Json(serde_json::Value::Object(map)))
    }
}

fn eval_literal(lit: &LiteralValue) -> Value {
    match lit {
        LiteralValue::Null => Value::Null,
        LiteralValue::Bool(b) => Value::Bool(*b),
        LiteralValue::Int(i) => Value::Int64(*i),
        LiteralValue::Float(f) => Value::Float64(*f),
        LiteralValue::String(s) => Value::Text(s.clone()),
    }
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => b.to_string(),
        Value::Int8(v) => v.to_string(),
        Value::Int16(v) => v.to_string(),
        Value::Int32(v) => v.to_string(),
        Value::Int64(v) => v.to_string(),
        Value::UInt8(v) => v.to_string(),
        Value::UInt16(v) => v.to_string(),
        Value::UInt32(v) => v.to_string(),
        Value::UInt64(v) => v.to_string(),
        Value::Float32(v) => v.to_string(),
        Value::Float64(v) => v.to_string(),
        Value::Text(s) => s.clone(),
        Value::BigInt(v) => v.to_string(),
        Value::Decimal(v) => v.to_string(),
        Value::Uuid(v) => v.to_string(),
        Value::Date(d) => d.to_string(),
        Value::Timestamp(t) => t.to_rfc3339(),
        Value::Bytes(b) => format!("{b:?}"),
        Value::Ipv4(v) => v.to_string(),
        Value::Ipv6(v) => v.to_string(),
        Value::Json(v) => v.to_string(),
        Value::Object(entries) => {
            let map: serde_json::Map<String, serde_json::Value> = entries
                .iter()
                .map(|(k, v)| (k.clone(), serde_json::Value::String(value_to_string(v))))
                .collect();
            serde_json::Value::Object(map).to_string()
        }
        Value::Custom(v) => format!("{v:?}"),
    }
}

fn value_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Bool(b) => serde_json::Value::Bool(*b),
        Value::Int8(v) => serde_json::Value::Number((*v as i64).into()),
        Value::Int16(v) => serde_json::Value::Number((*v as i64).into()),
        Value::Int32(v) => serde_json::Value::Number((*v as i64).into()),
        Value::Int64(v) => serde_json::Value::Number((*v).into()),
        Value::UInt8(v) => serde_json::Value::Number((*v as u64).into()),
        Value::UInt16(v) => serde_json::Value::Number((*v as u64).into()),
        Value::UInt32(v) => serde_json::Value::Number((*v as u64).into()),
        Value::UInt64(v) => serde_json::Value::Number((*v).into()),
        Value::Float32(v) => serde_json::Number::from_f64(*v as f64)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Float64(v) => serde_json::Number::from_f64(*v)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Value::Text(s) => serde_json::Value::String(s.clone()),
        Value::BigInt(v) => serde_json::Value::String(v.to_string()),
        Value::Decimal(v) => serde_json::Value::String(v.to_string()),
        Value::Uuid(v) => serde_json::Value::String(v.to_string()),
        Value::Date(d) => serde_json::Value::String(d.to_string()),
        Value::Timestamp(t) => serde_json::Value::String(t.to_rfc3339()),
        Value::Bytes(b) => serde_json::Value::String(format!("{b:?}")),
        Value::Ipv4(v) => serde_json::Value::String(v.to_string()),
        Value::Ipv6(v) => serde_json::Value::String(v.to_string()),
        Value::Json(v) => v.clone(),
        Value::Object(entries) => {
            let map: serde_json::Map<String, serde_json::Value> = entries
                .iter()
                .map(|(k, v)| (k.clone(), value_to_json(v)))
                .collect();
            serde_json::Value::Object(map)
        }
        Value::Custom(v) => serde_json::Value::String(format!("{v:?}")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
    use air_elt_expr_funcs::{FuncError, FunctionRegistry};
    use air_elt_types::Value;

    use super::*;
    use crate::parser::parse;

    struct TestEnv {
        vars: AHashMap<String, String>,
    }

    impl TestEnv {
        fn new() -> Self {
            Self {
                vars: AHashMap::new(),
            }
        }

        fn with_var(mut self, key: &str, value: &str) -> Self {
            self.vars.insert(key.to_string(), value.to_string());
            self
        }
    }

    impl EnvResolver for TestEnv {
        fn get(&self, key: &str) -> Option<String> {
            self.vars.get(key).cloned()
        }
    }

    struct NoopFiles;

    impl FileResolver for NoopFiles {
        fn read(&self, path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
            Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: "not implemented".to_owned(),
            })
        }
    }

    fn test_context() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(TestEnv::new()),
            file_resolver: Arc::new(NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::from("/tmp"),
        }
    }

    fn test_context_with_env(env: TestEnv) -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(env),
            file_resolver: Arc::new(NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::from("/tmp"),
        }
    }

    fn eval(input: &str) -> Result<Value, ExprError> {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        eval_expression(input, &registry, &context)
    }

    #[test]
    fn literal_null() {
        assert_eq!(eval("null").unwrap(), Value::Null);
    }

    #[test]
    fn literal_bool_true() {
        assert_eq!(eval("true").unwrap(), Value::Bool(true));
    }

    #[test]
    fn literal_bool_false() {
        assert_eq!(eval("false").unwrap(), Value::Bool(false));
    }

    #[test]
    fn literal_int() {
        assert_eq!(eval("42").unwrap(), Value::Int64(42));
    }

    #[test]
    fn literal_float() {
        assert_eq!(eval("2.71").unwrap(), Value::Float64(2.71));
    }

    #[test]
    fn literal_string() {
        assert_eq!(eval("'hello'").unwrap(), Value::Text("hello".to_string()));
    }

    #[test]
    fn arithmetic_addition() {
        assert_eq!(eval("1 + 2").unwrap(), Value::Int64(3));
    }

    #[test]
    fn arithmetic_division() {
        assert_eq!(eval("10 / 3").unwrap(), Value::Int64(3));
    }

    #[test]
    fn arithmetic_float_multiply() {
        assert_eq!(eval("2.5 * 2.0").unwrap(), Value::Float64(5.0));
    }

    #[test]
    fn string_concat_via_add() {
        assert_eq!(
            eval("'hello' + ' ' + 'world'").unwrap(),
            Value::Text("hello world".to_string())
        );
    }

    #[test]
    fn variable_binding() {
        assert_eq!(eval("x = 5; x + 1").unwrap(), Value::Int64(6));
    }

    #[test]
    fn multiple_variable_bindings() {
        assert_eq!(eval("x = 3; y = 4; x + y").unwrap(), Value::Int64(7));
    }

    #[test]
    fn function_call_if() {
        assert_eq!(
            eval("if(true, 'yes', 'no')").unwrap(),
            Value::Text("yes".to_string())
        );
    }

    #[test]
    fn function_call_if_false() {
        assert_eq!(
            eval("if(false, 'yes', 'no')").unwrap(),
            Value::Text("no".to_string())
        );
    }

    #[test]
    fn nested_function_calls() {
        assert_eq!(
            eval("concat(toString(1 + 2), '!')").unwrap(),
            Value::Text("3!".to_string())
        );
    }

    #[test]
    fn string_interpolation() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let program = parse("\"value: {1 + 2}\"").unwrap();
        let result = evaluate(&program, &registry, &context).unwrap();
        assert_eq!(result, Value::Text("value: 3".to_string()));
    }

    #[test]
    fn null_propagation() {
        assert_eq!(eval("1 + null").unwrap(), Value::Null);
    }

    #[test]
    fn coalesce_function() {
        assert_eq!(eval("coalesce(null, null, 42)").unwrap(), Value::Int64(42));
    }

    #[test]
    fn coalesce_first_non_null() {
        assert_eq!(eval("coalesce(null, 7, 42)").unwrap(), Value::Int64(7));
    }

    #[test]
    fn object_literal() {
        let result = eval("{\"key\" = 1 + 1}").unwrap();
        match result {
            Value::Json(obj) => {
                assert_eq!(obj.get("key"), Some(&serde_json::json!(2)));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn object_literal_multiple_keys() {
        let result = eval("{\"a\" = 1, \"b\" = 'hello'}").unwrap();
        match result {
            Value::Json(obj) => {
                assert_eq!(obj.get("a"), Some(&serde_json::json!(1)));
                assert_eq!(obj.get("b"), Some(&serde_json::json!("hello")));
            }
            other => panic!("expected Json, got {other:?}"),
        }
    }

    #[test]
    fn env_resolver_with_value() {
        let registry = FunctionRegistry::with_builtins();
        let env = TestEnv::new().with_var("MY_KEY", "my_value");
        let context = test_context_with_env(env);
        let result = eval_expression("env('MY_KEY')", &registry, &context).unwrap();
        assert_eq!(result, Value::Text("my_value".to_string()));
    }

    #[test]
    fn undefined_variable_error() {
        let result = eval("unknown_var + 1");
        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::UndefinedVariable { name } => {
                assert_eq!(name, "unknown_var");
            }
            other => panic!("expected UndefinedVariable, got {other:?}"),
        }
    }

    #[test]
    fn unknown_function_error() {
        let result = eval("nonexistent_func(1, 2)");
        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::Function(FuncError::UnknownFunction { name }) => {
                assert_eq!(name, "nonexistent_func");
            }
            other => panic!("expected Function(UnknownFunction), got {other:?}"),
        }
    }

    #[test]
    fn eval_interpolated_basic() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let result = eval_interpolated("hello {1 + 2} world", &registry, &context).unwrap();
        assert_eq!(result, "hello 3 world");
    }

    #[test]
    fn eval_interpolated_escaped_braces() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let result = eval_interpolated("no {{interpolation}}", &registry, &context).unwrap();
        assert_eq!(result, "no {interpolation}");
    }

    #[test]
    fn eval_interpolated_no_markers() {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let result = eval_interpolated("plain text", &registry, &context).unwrap();
        assert_eq!(result, "plain text");
    }

    #[test]
    fn nesting_depth_limit() {
        // Build a deeply nested function call: f(f(f(...(1)...)))
        // where f is toString — each adds a depth level
        let mut input = String::new();
        for _ in 0..(MAX_EXPR_DEPTH + 1) {
            input.push_str("toString(");
        }
        input.push('1');
        for _ in 0..(MAX_EXPR_DEPTH + 1) {
            input.push(')');
        }

        let result = std::thread::Builder::new()
            .stack_size(8 * 1024 * 1024)
            .spawn(move || eval(&input))
            .expect("failed to spawn thread")
            .join()
            .expect("thread panicked");

        assert!(result.is_err());
        match result.unwrap_err() {
            ExprError::NestingTooDeep { max } => {
                assert_eq!(max, MAX_EXPR_DEPTH);
            }
            other => panic!("expected NestingTooDeep, got {other:?}"),
        }
    }

    #[test]
    fn comparison_operators() {
        assert_eq!(eval("1 == 1").unwrap(), Value::Bool(true));
        assert_eq!(eval("1 != 2").unwrap(), Value::Bool(true));
        assert_eq!(eval("1 < 2").unwrap(), Value::Bool(true));
        assert_eq!(eval("2 > 1").unwrap(), Value::Bool(true));
        assert_eq!(eval("1 <= 1").unwrap(), Value::Bool(true));
        assert_eq!(eval("1 >= 1").unwrap(), Value::Bool(true));
    }

    #[test]
    fn logical_operators() {
        assert_eq!(eval("true && true").unwrap(), Value::Bool(true));
        assert_eq!(eval("true && false").unwrap(), Value::Bool(false));
        assert_eq!(eval("false || true").unwrap(), Value::Bool(true));
        assert_eq!(eval("!false").unwrap(), Value::Bool(true));
    }

    #[test]
    fn negation() {
        assert_eq!(eval("-5").unwrap(), Value::Int64(-5));
        assert_eq!(eval("-2.5").unwrap(), Value::Float64(-2.5));
    }

    #[test]
    fn variable_shadowing_in_order() {
        assert_eq!(eval("x = 1; x = x + 1; x").unwrap(), Value::Int64(2));
    }
}
