use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_parse::model::{
    ConditionalExpr, Expr, InterpolationSegment, LiteralValue, Program, Statement,
};
use air_elt_expr_types::limits::MAX_EXPR_DEPTH;
use air_elt_types::Value;

use crate::context::ExpressionContext;
use crate::error::ExprError;

pub struct Evaluator<'a> {
    context: &'a ExpressionContext,
}

impl<'a> Evaluator<'a> {
    pub fn create(context: &'a ExpressionContext) -> Self {
        Self { context }
    }

    pub fn evaluate(&self, program: &Program) -> Result<Value, ExprError> {
        let mut state = EvaluatorState::new(&self.context.registry, &self.context.eval_context);
        state.eval_program(program)
    }
}

#[cfg(test)]
fn evaluate(
    program: &Program,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Value, ExprError> {
    let mut state = EvaluatorState::new(registry, context);
    state.eval_program(program)
}

struct EvaluatorState<'a> {
    registry: &'a FunctionRegistry,
    context: &'a EvalContext,
    variables: AHashMap<String, Value>,
    depth: usize,
}

impl<'a> EvaluatorState<'a> {
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
            Expr::Conditional(conditional) => self.eval_conditional(conditional),
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
            return Err(air_elt_expr_parse::ExprError::NestingTooDeep {
                max: MAX_EXPR_DEPTH,
            }
            .into());
        }

        let func_ref = self.registry.get_ref(name, Some(args.len()))?;
        let function = self.registry.get_by_ref(func_ref);

        let mut evaluated_args = Vec::with_capacity(args.len());
        for arg in args {
            evaluated_args.push(self.eval_expr(arg)?);
        }

        let result = function.evaluate(evaluated_args, self.context)?;
        self.depth -= 1;
        Ok(result)
    }

    fn eval_conditional(&mut self, conditional: &ConditionalExpr) -> Result<Value, ExprError> {
        self.depth += 1;
        if self.depth > MAX_EXPR_DEPTH {
            self.depth -= 1;
            return Err(air_elt_expr_parse::ExprError::NestingTooDeep {
                max: MAX_EXPR_DEPTH,
            }
            .into());
        }

        let result = match conditional {
            ConditionalExpr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let cond_value = self.eval_expr(condition)?;
                match cond_value {
                    Value::Bool(true) => self.eval_expr(then_branch)?,
                    Value::Bool(false) | Value::Null => self.eval_expr(else_branch)?,
                    other => {
                        return Err(ExprError::Function(
                            air_elt_expr_funcs::FuncError::TypeMismatch {
                                function: "if".to_owned(),
                                expected: "Bool".to_owned(),
                                actual: format!("{:?}", other.data_type()),
                            },
                        ));
                    }
                }
            }
            ConditionalExpr::MultiIf { branches, default } => {
                let mut found = None;
                for (condition, value) in branches {
                    let cond_value = self.eval_expr(condition)?;
                    match cond_value {
                        Value::Bool(true) => {
                            found = Some(self.eval_expr(value)?);
                            break;
                        }
                        Value::Bool(false) | Value::Null => continue,
                        other => {
                            return Err(ExprError::Function(
                                air_elt_expr_funcs::FuncError::TypeMismatch {
                                    function: "multiIf".to_owned(),
                                    expected: "Bool".to_owned(),
                                    actual: format!("{:?}", other.data_type()),
                                },
                            ));
                        }
                    }
                }
                found.map_or_else(|| self.eval_expr(default), Ok)?
            }
            ConditionalExpr::IfNull { value, alternative } => {
                let val = self.eval_expr(value)?;
                if val.is_null() {
                    self.eval_expr(alternative)?
                } else {
                    val
                }
            }
            ConditionalExpr::NullIf { value, sentinel } => {
                let val = self.eval_expr(value)?;
                let sent = self.eval_expr(sentinel)?;
                if val == sent { Value::Null } else { val }
            }
            ConditionalExpr::And { left, right } => {
                let left_val = self.eval_expr(left)?;
                match left_val {
                    Value::Bool(false) => Value::Bool(false),
                    Value::Bool(true) => {
                        let right_val = self.eval_expr(right)?;
                        match right_val {
                            Value::Null => Value::Null,
                            Value::Bool(b) => Value::Bool(b),
                            other => {
                                return Err(ExprError::Function(
                                    air_elt_expr_funcs::FuncError::TypeMismatch {
                                        function: "and".to_owned(),
                                        expected: "Bool".to_owned(),
                                        actual: format!("{:?}", other.data_type()),
                                    },
                                ));
                            }
                        }
                    }
                    Value::Null => {
                        // SQL three-valued: NULL AND FALSE = FALSE, NULL AND TRUE/NULL = NULL
                        let right_val = self.eval_expr(right)?;
                        match right_val {
                            Value::Bool(false) => Value::Bool(false),
                            Value::Bool(true) | Value::Null => Value::Null,
                            other => {
                                return Err(ExprError::Function(
                                    air_elt_expr_funcs::FuncError::TypeMismatch {
                                        function: "and".to_owned(),
                                        expected: "Bool".to_owned(),
                                        actual: format!("{:?}", other.data_type()),
                                    },
                                ));
                            }
                        }
                    }
                    other => {
                        return Err(ExprError::Function(
                            air_elt_expr_funcs::FuncError::TypeMismatch {
                                function: "and".to_owned(),
                                expected: "Bool".to_owned(),
                                actual: format!("{:?}", other.data_type()),
                            },
                        ));
                    }
                }
            }
            ConditionalExpr::Or { left, right } => {
                let left_val = self.eval_expr(left)?;
                match left_val {
                    Value::Bool(true) => Value::Bool(true),
                    Value::Bool(false) => {
                        let right_val = self.eval_expr(right)?;
                        match right_val {
                            Value::Null => Value::Null,
                            Value::Bool(b) => Value::Bool(b),
                            other => {
                                return Err(ExprError::Function(
                                    air_elt_expr_funcs::FuncError::TypeMismatch {
                                        function: "or".to_owned(),
                                        expected: "Bool".to_owned(),
                                        actual: format!("{:?}", other.data_type()),
                                    },
                                ));
                            }
                        }
                    }
                    Value::Null => {
                        // SQL three-valued: NULL OR TRUE = TRUE, NULL OR FALSE/NULL = NULL
                        let right_val = self.eval_expr(right)?;
                        match right_val {
                            Value::Bool(true) => Value::Bool(true),
                            Value::Bool(false) | Value::Null => Value::Null,
                            other => {
                                return Err(ExprError::Function(
                                    air_elt_expr_funcs::FuncError::TypeMismatch {
                                        function: "or".to_owned(),
                                        expected: "Bool".to_owned(),
                                        actual: format!("{:?}", other.data_type()),
                                    },
                                ));
                            }
                        }
                    }
                    other => {
                        return Err(ExprError::Function(
                            air_elt_expr_funcs::FuncError::TypeMismatch {
                                function: "or".to_owned(),
                                expected: "Bool".to_owned(),
                                actual: format!("{:?}", other.data_type()),
                            },
                        ));
                    }
                }
            }
        };

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
            if result.len() > air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES {
                return Err(ExprError::Function(
                    air_elt_expr_funcs::FuncError::StringTooLarge {
                        len: result.len(),
                        max: air_elt_expr_types::limits::MAX_EXPR_STRING_BYTES,
                    },
                ));
            }
        }
        Ok(Value::Text(result))
    }

    fn eval_object(&mut self, entries: &[(String, Expr)]) -> Result<Value, ExprError> {
        let mut map = serde_json::Map::with_capacity(entries.len());
        for (key, value_expr) in entries {
            let value = self.eval_expr(value_expr)?;
            let json_val = air_elt_types::value_to_json(&value).unwrap_or(serde_json::Value::Null);
            map.insert(key.clone(), json_val);
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
                .map(|(k, v)| {
                    (
                        k.clone(),
                        air_elt_types::value_to_json(v).unwrap_or(serde_json::Value::Null),
                    )
                })
                .collect();
            serde_json::Value::Object(map).to_string()
        }
        Value::Custom(v) => format!("{v:?}"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
    use air_elt_expr_funcs::{FuncError, FunctionRegistry};
    use air_elt_expr_parse::Parser;
    use air_elt_expr_types::limits::MAX_EXPR_DEPTH;
    use air_elt_types::Value;

    use super::*;

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
            is_compile_time: false,
        }
    }

    fn test_context_with_env(env: TestEnv) -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(env),
            file_resolver: Arc::new(NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::from("/tmp"),
            is_compile_time: false,
        }
    }

    fn eval(input: &str) -> Result<Value, ExprError> {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let program = Parser::create().parse_expression(input)?;
        evaluate(&program, &registry, &context)
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
        assert_eq!(
            eval("\"value: {1 + 2}\"").unwrap(),
            Value::Text("value: 3".to_string())
        );
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
        let program = Parser::create().parse_expression("env('MY_KEY')").unwrap();
        let result = evaluate(&program, &registry, &context).unwrap();
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

    fn eval_interpolation_template(input: &str) -> Result<Value, ExprError> {
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let program = Parser::create().parse(input)?;
        evaluate(&program, &registry, &context)
    }

    #[test]
    fn eval_interpolated_basic() {
        assert_eq!(
            eval_interpolation_template("hello {1 + 2} world").unwrap(),
            Value::Text("hello 3 world".to_string())
        );
    }

    #[test]
    fn eval_interpolated_escaped_braces() {
        // No unescaped `{...}` markers — `Parser::parse` returns the literal verbatim.
        // (Escape unwrapping only happens when the parser sees the string as an
        // interpolation template, which requires at least one real interpolation marker.)
        assert_eq!(
            eval_interpolation_template("no {{interpolation}}").unwrap(),
            Value::Text("no {{interpolation}}".to_string())
        );
    }

    #[test]
    fn eval_interpolated_no_markers() {
        // Plain text — parser returns it as a literal string.
        assert_eq!(
            eval_interpolation_template("plain text").unwrap(),
            Value::Text("plain text".to_string())
        );
    }

    #[test]
    fn nesting_depth_limit() {
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
            ExprError::Parse(air_elt_expr_parse::ExprError::NestingTooDeep { max }) => {
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

    #[test]
    fn if_true_skips_else_branch() {
        let result = eval("if(true, 42, 1/0)").unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn if_false_skips_then_branch() {
        let result = eval("if(false, 1/0, 42)").unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn coalesce_stops_at_first_non_null() {
        let result = eval("coalesce(null, 42, 1/0)").unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn multi_if_skips_unreached_branches() {
        let result = eval("multiIf(true, 42, 1/0 == 1, 1/0, 1/0)").unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn and_false_skips_right() {
        let result = eval("false && (1/0 == 1)").unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn or_true_skips_right() {
        let result = eval("true || (1/0 == 1)").unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn if_null_non_null_skips_alternative() {
        let result = eval("ifNull(42, 1/0)").unwrap();
        assert_eq!(result, Value::Int64(42));
    }

    #[test]
    fn null_if_basic() {
        assert_eq!(eval("nullIf(1, 1)").unwrap(), Value::Null);
        assert_eq!(eval("nullIf(1, 2)").unwrap(), Value::Int64(1));
    }

    #[test]
    fn interpolation_non_ascii() {
        assert_eq!(
            eval_interpolation_template("Привет {1 + 1} мир").unwrap(),
            Value::Text("Привет 2 мир".to_string())
        );
    }

    #[test]
    fn interpolation_emoji() {
        assert_eq!(
            eval_interpolation_template("🎉{42}🎊").unwrap(),
            Value::Text("🎉42🎊".to_string())
        );
    }

    #[test]
    fn or_null_true_is_true() {
        let result = eval("null || true").unwrap();
        assert_eq!(result, Value::Bool(true));
    }

    #[test]
    fn or_null_false_is_null() {
        let result = eval("null || false").unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn or_null_null_is_null() {
        let result = eval("null || null").unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn and_null_false_is_false() {
        let result = eval("null && false").unwrap();
        assert_eq!(result, Value::Bool(false));
    }

    #[test]
    fn and_null_true_is_null() {
        let result = eval("null && true").unwrap();
        assert_eq!(result, Value::Null);
    }

    #[test]
    fn power_operator_basic() {
        let result = eval("2 ** 3").unwrap();
        assert_eq!(result, Value::Int64(8));
    }

    #[test]
    fn power_operator_right_associative() {
        let result = eval("2 ** 3 ** 2").unwrap();
        assert_eq!(result, Value::Int64(512));
    }

    #[test]
    fn replace_all_occurrences() {
        let result = eval("replace('aaa', 'a', 'b')").unwrap();
        assert_eq!(result, Value::Text("bbb".to_owned()));
    }

    #[test]
    fn interpolation_size_cap_rejects_oversized_result() {
        let input = "{repeat('x', 600000)}{repeat('y', 600000)}";
        let err = eval_interpolation_template(input).unwrap_err();
        assert!(
            matches!(
                err,
                ExprError::Function(air_elt_expr_funcs::FuncError::StringTooLarge { .. })
            ),
            "expected StringTooLarge, got {err:?}"
        );
    }

    #[test]
    fn evaluator_struct_basic() {
        let registry = Arc::new(FunctionRegistry::with_builtins());
        let expr_ctx = ExpressionContext::create(registry, std::path::Path::new("/tmp"));
        let evaluator = Evaluator::create(&expr_ctx);
        let program = Parser::create().parse_expression("1 + 2").unwrap();
        let result = evaluator.evaluate(&program).unwrap();
        assert_eq!(result, Value::Int64(3));
    }

    #[test]
    fn evaluator_struct_interpolated() {
        let registry = Arc::new(FunctionRegistry::with_builtins());
        let expr_ctx = ExpressionContext::create(registry, std::path::Path::new("/tmp"));
        let evaluator = Evaluator::create(&expr_ctx);
        let program = Parser::create().parse("hello {1 + 2} world").unwrap();
        let result = evaluator.evaluate(&program).unwrap();
        assert_eq!(result, Value::Text("hello 3 world".to_string()));
    }
}
