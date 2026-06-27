use ahash::AHashMap;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_funcs::{FunctionRegistry, SliceArgWindow};
use air_elt_expr_parse::model::{
    ConditionalExpr, Expr, InterpolationSegment, LiteralValue, Program, Statement,
};
use air_elt_expr_types::limits::MAX_EXPR_DEPTH;
use air_elt_types::Value;
use air_elt_types::value_to_string;

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
    /// Shared per-program argument stack. Each call pushes its evaluated
    /// arguments above `base = arg_stack.len()` (nested calls stack on top) and
    /// truncates back once the function returns, so the allocation is reused
    /// across every call instead of a per-call vector.
    arg_stack: Vec<Value>,
}

impl<'a> EvaluatorState<'a> {
    fn new(registry: &'a FunctionRegistry, context: &'a EvalContext) -> Self {
        Self {
            registry,
            context,
            variables: AHashMap::new(),
            depth: 0,
            arg_stack: Vec::new(),
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
            Expr::Array(elements) => self.eval_array(elements),
            Expr::Block { statements, result } => self.eval_block(statements, result),
            Expr::Field(..) | Expr::Fields(..) => Err(ExprError::FieldOutsideTransform),
        }
    }

    /// Evaluate a scoped-binding block (`{ name = expr; …; result }` in an
    /// `if`-branch position). Each binding evaluates once at its binding point
    /// and shadows its name for the remainder of the block; after the result is
    /// evaluated the displaced entries are restored in reverse insertion order
    /// (fresh names removed). The restore runs on both the success and error
    /// paths so the outer scope and sibling branches see the pre-block bindings
    /// again. A binding lives in the same depth-guarded recursion as any other
    /// expression — the guard is enforced by the `eval_conditional` arm that
    /// reaches this block, mirroring the evaluator's existing pattern.
    fn eval_block(&mut self, statements: &[Statement], result: &Expr) -> Result<Value, ExprError> {
        let mut displaced: Vec<(&str, Option<Value>)> = Vec::with_capacity(statements.len());
        let outcome = self.eval_block_scope(statements, result, &mut displaced);

        // Entries borrow the AST names; only a shadowed restore re-owns one.
        for (name, previous) in displaced.into_iter().rev() {
            match previous {
                Some(value) => {
                    self.variables.insert(name.to_owned(), value);
                }
                None => {
                    self.variables.remove(name);
                }
            }
        }

        outcome
    }

    /// The scope-mutating part of block evaluation: evaluate each binding,
    /// install it into the variable map (recording the displaced entry in
    /// insertion order, even on a later error), then evaluate the result.
    fn eval_block_scope<'s>(
        &mut self,
        statements: &'s [Statement],
        result: &Expr,
        displaced: &mut Vec<(&'s str, Option<Value>)>,
    ) -> Result<Value, ExprError> {
        for statement in statements {
            let value = self.eval_expr(&statement.value)?;
            let previous = self.variables.insert(statement.name.clone(), value);
            displaced.push((&statement.name, previous));
        }
        self.eval_expr(result)
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

        // Evaluate the arguments onto the shared argument stack (nested calls
        // stack above `base`), then hand the function a window over that slice —
        // no per-call allocation. The stack is truncated back to `base` whether
        // the function succeeds or fails.
        let base = self.arg_stack.len();
        for arg in args {
            let value = self.eval_expr(arg)?;
            self.arg_stack.push(value);
        }

        let outcome = {
            let mut window = SliceArgWindow::create(&mut self.arg_stack[base..]);
            function.evaluate(&mut window, self.context)
        };
        self.arg_stack.truncate(base);
        let result = outcome?;
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
                    // Mirror the arena evaluator: append Text directly instead
                    // of cloning it through `value_to_string`.
                    match &value {
                        Value::Text(text) => result.push_str(text),
                        other => result.push_str(&value_to_string(other)),
                    }
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
        let mut fields = Vec::with_capacity(entries.len());
        for (key, value_expr) in entries {
            let value = self.eval_expr(value_expr)?;
            fields.push((key.clone(), value));
        }
        Ok(Value::Object(fields))
    }

    fn eval_array(&mut self, elements: &[Expr]) -> Result<Value, ExprError> {
        let mut values = Vec::with_capacity(elements.len());
        for element in elements {
            values.push(self.eval_expr(element)?);
        }
        Ok(Value::Array(values))
    }
}

fn eval_literal(lit: &LiteralValue) -> Value {
    match lit {
        LiteralValue::Null => Value::Null,
        LiteralValue::Bool(b) => Value::Bool(*b),
        LiteralValue::Int(i) => Value::Int64(*i),
        LiteralValue::Float(f) => Value::Float64(*f),
        LiteralValue::String(s) => Value::Text(s.clone()),
        LiteralValue::Interval(d) => Value::Interval(*d),
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
            caches: air_elt_expr_funcs::ExprCaches::default(),
        }
    }

    fn test_context_with_env(env: TestEnv) -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(env),
            file_resolver: Arc::new(NoopFiles),
            now: chrono::Utc::now(),
            base_dir: PathBuf::from("/tmp"),
            is_compile_time: false,
            caches: air_elt_expr_funcs::ExprCaches::default(),
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
    fn literal_duration() {
        use std::time::Duration;
        assert_eq!(
            eval("10s").unwrap(),
            Value::Interval(Duration::from_secs(10))
        );
        assert_eq!(
            eval("1h30m").unwrap(),
            Value::Interval(Duration::from_secs(5400))
        );
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
        assert_eq!(
            result,
            Value::Object(vec![("key".to_string(), Value::Int64(2))])
        );
    }

    #[test]
    fn object_literal_multiple_keys() {
        let result = eval("{\"a\" = 1, \"b\" = 'hello'}").unwrap();
        assert_eq!(
            result,
            Value::Object(vec![
                ("a".to_string(), Value::Int64(1)),
                ("b".to_string(), Value::Text("hello".to_string())),
            ])
        );
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

    // --- if/else-if/else with brace blocks ------------------------------------

    #[test]
    fn block_then_branch_value() {
        assert_eq!(
            eval("if (true) { x = 2; x + 1 } else 0").unwrap(),
            Value::Int64(3)
        );
    }

    #[test]
    fn block_else_branch_value() {
        assert_eq!(
            eval("if (false) 0 else { x = 5; x * 2 }").unwrap(),
            Value::Int64(10)
        );
    }

    #[test]
    fn block_nested_blocks() {
        assert_eq!(
            eval("if (true) { x = 1; if (true) { y = 2; x + y } else 0 } else 9").unwrap(),
            Value::Int64(3)
        );
    }

    #[test]
    fn block_else_if_chain() {
        let source = "if (false) { a = 1; a } else if (true) { b = 2; b + 10 } else { c = 3; c }";
        assert_eq!(eval(source).unwrap(), Value::Int64(12));
    }

    #[test]
    fn block_laziness_else_not_evaluated() {
        // The failing else-branch block (1 / 0) must never run when the
        // condition is true.
        assert_eq!(
            eval("if (true) 1 else { x = 1 / 0; x }").unwrap(),
            Value::Int64(1)
        );
    }

    #[test]
    fn block_laziness_then_not_evaluated() {
        // The failing then-branch block must never run when the condition is
        // false.
        assert_eq!(
            eval("if (false) { x = 1 / 0; x } else 1").unwrap(),
            Value::Int64(1)
        );
    }

    #[test]
    fn block_shadowing_restores_outer() {
        // Inner `x = 2` shadows the outer `x = 1` only inside the block; after
        // the block `x` is 1 again, so `x + y` = 1 + 12 = 13.
        let source = "x = 1; y = if (true) { x = 2; x + 10 } else 0; x + y";
        assert_eq!(eval(source).unwrap(), Value::Int64(13));
    }

    #[test]
    fn block_sibling_branches_same_name() {
        // Each branch binds `n` in its own scope; the not-taken branch never
        // runs, and neither name leaks out.
        let source = "if (true) { n = 1; n } else { n = 2; n }";
        assert_eq!(eval(source).unwrap(), Value::Int64(1));
    }

    #[test]
    fn block_nested_shadowing() {
        let source = "x = 1; if (true) { x = 2; if (true) { x = 3; x } else 0 } else 0";
        assert_eq!(eval(source).unwrap(), Value::Int64(3));
    }

    #[test]
    fn block_local_name_undefined_after_if() {
        // `t` is block-local; referencing it after the if is an
        // undefined-variable error.
        let source = "y = if (true) { t = 5; t } else 0; t + y";
        let err = eval(source).unwrap_err();
        match err {
            ExprError::UndefinedVariable { name } => assert_eq!(name, "t"),
            other => panic!("expected UndefinedVariable, got {other:?}"),
        }
    }

    #[test]
    fn or_over_non_bool_errors_on_both_paths() {
        // Proptest counterexample (2026-06-10): `false || <non-bool>` must fail
        // on the arena path exactly like the heap path. BranchPrune used to
        // fold `false || x` to bare `x`, erasing the evaluator's "right
        // operand must be Bool/Null" check; it now folds to TypeAssert{Bool}.
        let registry = FunctionRegistry::with_builtins();
        let context = test_context();
        let cases = [
            "false || 5",
            "true && 'x'",
            // The original minimized failing input: the inner if folds to an
            // Int that lands as the right operand of `||`.
            "if((max(0, 0) < ((2 < 0) || (if ((0 < 1)) { t = 0; (t + t) } else 0 < 0))), \
             max(0, 0), 0)",
        ];
        for source in cases {
            let program = Parser::create().parse_expression(source).unwrap();
            let heap = evaluate(&program, &registry, &context).map_err(|_| ());
            let arena = arena_eval(&program, &registry, &context);
            assert!(heap.is_err(), "heap accepted `{source}`: {heap:?}");
            assert!(arena.is_err(), "arena accepted `{source}`: {arena:?}");
        }
    }

    #[test]
    fn block_binding_evaluated_once() {
        // Two reads of the same binding observe the same value; with an impure
        // binding this also pins the single-evaluation contract.
        let source = "if (true) { v = randomInt(0, 1000000); v == v } else false";
        assert_eq!(eval(source).unwrap(), Value::Bool(true));
    }

    // --- Differential oracle: heap (AST) evaluator vs arena evaluator ---------
    //
    // Production const / default / switch / patch evaluation runs through the
    // optimizer's arena evaluator (`compile` → `RuntimeProgram::evaluate`). This
    // proptest pins that path to the original heap AST evaluator retained here:
    // for any field-free program they must produce the same value (or both
    // error). This is the gate that let the patcher and core call-sites migrate
    // off the heap evaluator.

    use air_elt_expr_optimize::Optimizer;
    use proptest::prelude::*;

    use crate::program::RuntimeProgram;

    /// Random field-free (comptime) expressions: numeric / string / bool / null
    /// and impure (`now`/`env`) leaves under arithmetic, comparison, Kleene
    /// logic, conditional, null-handling, and interpolation ops — the shapes the
    /// const / default / switch / patch path actually evaluates. The impure
    /// leaves matter: the optimizer leaves `now`/`env` unfolded, so the arena
    /// evaluator runs them per the same `EvalContext` the heap evaluator uses,
    /// and the oracle pins that they still agree.
    fn const_expression() -> impl Strategy<Value = String> {
        let leaf = prop_oneof![
            6 => (0i64..20).prop_map(|n| n.to_string()),
            2 => (0i64..20).prop_map(|n| format!("'{n}'")),
            1 => Just("true".to_owned()),
            1 => Just("false".to_owned()),
            1 => Just("null".to_owned()),
            1 => Just("now()".to_owned()),
            1 => Just("env('AIR_ELT_ABSENT', 'fallback')".to_owned()),
        ];
        leaf.prop_recursive(4, 64, 3, |inner| {
            // A boolean sub-expression, so `&&` / `||` exercise real Kleene logic
            // (not just a non-bool type error on both sides).
            let boolean = (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} < {b})"));
            prop_oneof![
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("min({a}, {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("max({a}, {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("ifNull({a}, {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("nullIf({a}, {b})")),
                (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} == {b})")),
                inner.clone().prop_map(|a| format!("(-{a})")),
                (boolean.clone(), boolean.clone()).prop_map(|(a, b)| format!("({a} && {b})")),
                (boolean.clone(), boolean.clone()).prop_map(|(a, b)| format!("({a} || {b})")),
                inner.clone().prop_map(|a| format!("\"x{{{a}}}y\"")),
                // Single-key object literal (distinct key → never a duplicate-key
                // compile error) and its round-trip through `objectGet`, so the
                // oracle also pins `Value::Object` construction across heap/arena.
                inner.clone().prop_map(|a| format!("{{\"k\" = {a}}}")),
                inner
                    .clone()
                    .prop_map(|a| format!("objectGet({{\"k\" = {a}}}, \"k\")")),
                (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(a, b, c)| format!("if(({a} < {b}), {a}, {c})")),
                // New if/else surface form: `if (cond) a else b`.
                (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(a, b, c)| format!("if (({a} < {b})) {a} else {c}")),
                // New if/else with a brace block in the then-branch: a block
                // binds a name once and reuses it in its mandatory result.
                (inner.clone(), inner.clone(), inner.clone()).prop_map(|(a, b, c)| format!(
                    "if (({a} < {b})) {{ t = {a}; (t + t) }} else {c}"
                )),
            ]
        })
    }

    fn arena_eval(
        program: &air_elt_expr_parse::Program,
        registry: &FunctionRegistry,
        context: &EvalContext,
    ) -> Result<Value, ()> {
        // A compile error means the optimizer hit a constant that fails in an
        // eager position; the heap evaluator must then fail too (checked below).
        let compact = Optimizer::create(registry, context)
            .compile(program, None, None)
            .map_err(|_| ())?;
        RuntimeProgram::create(compact)
            .evaluate(registry, context)
            .map_err(|_| ())
    }

    /// Programs that bind an **impure** value to a variable and reuse it several
    /// times inside a single call. The binding must be impure so it is NOT
    /// const-folded away (a folded constant would reach the call as `Const`, not
    /// a register) yet **deterministic** across the heap and arena evaluators
    /// (both share `context.now`), so `toString(now())` / `toString(today())` fit
    /// while non-deterministic `random*` would falsely diverge. A variable read
    /// lowers to a register read, so reusing `v` in one call produces the same
    /// register aliasing that `field_hoist` creates for a repeated field —
    /// exercising the arena's register move/clone choice (`ArgStackItem::Register`
    /// vs `RegisterTake`) under the heap↔arena oracle, which the field-free
    /// `const_expression` cannot reach (the heap evaluator has no field binding).
    fn shared_register_program() -> impl Strategy<Value = String> {
        let bound = prop_oneof![
            Just("toString(now())".to_owned()),
            Just("toString(today())".to_owned()),
            Just("concat(toString(now()), 'q')".to_owned()),
        ];
        // Each reuses `v` >=2 times in one call, across ascending-take
        // (`concat`), descending-take (`replace`), and nested-move (`upper(v)`)
        // shapes — the cases the aliasing bug spanned.
        let shape = prop_oneof![
            Just("concat(v, v)"),
            Just("replace(v, v, v)"),
            Just("concat(v, upper(v))"),
            Just("concat(upper(v), v)"),
            Just("concat(v, concat(v, v))"),
            Just("slice(v, 0, 1)"),
        ];
        (bound, shape).prop_map(|(bound, shape)| format!("v = {bound}; {shape}"))
    }

    proptest! {
        /// The shared-register programs must evaluate identically on the heap and
        /// arena paths — the regression guard for hoisted/aliased register moves.
        #[test]
        fn heap_and_arena_agree_on_shared_registers(source in shared_register_program()) {
            let registry = FunctionRegistry::with_builtins();
            let context = test_context();
            let Ok(program) = Parser::create().parse_expression(&source) else {
                return Ok(());
            };
            let heap = evaluate(&program, &registry, &context).map_err(|_| ());
            let arena = arena_eval(&program, &registry, &context);
            match (heap, arena) {
                (Ok(left), Ok(right)) => prop_assert!(
                    left == right,
                    "value mismatch for `{source}`: {left:?} (heap) vs {right:?} (arena)"
                ),
                (Err(()), Err(())) => {}
                (left, right) => prop_assert!(
                    false,
                    "ok/err mismatch for `{source}`: {left:?} (heap) vs {right:?} (arena)"
                ),
            }
        }
    }

    proptest! {
        #[test]
        fn heap_and_arena_agree(source in const_expression()) {
            let registry = FunctionRegistry::with_builtins();
            let context = test_context();
            // The grammar can compose syntactically invalid sources (e.g. an
            // object literal as the whole content of an interpolation slot —
            // `"x{{"k"=0}}y"` — where the object's `{` collides with the `{{`
            // brace-escape). This oracle is an eval-equivalence check, so an
            // unparseable source is outside its domain: skip it.
            let Ok(program) = Parser::create().parse_expression(&source) else {
                return Ok(());
            };

            let heap = evaluate(&program, &registry, &context).map_err(|_| ());
            let arena = arena_eval(&program, &registry, &context);

            match (heap, arena) {
                (Ok(left), Ok(right)) => prop_assert!(
                    left == right,
                    "value mismatch for `{source}`: {left:?} (heap) vs {right:?} (arena)"
                ),
                (Err(()), Err(())) => {}
                (left, right) => prop_assert!(
                    false,
                    "ok/err mismatch for `{source}`: {left:?} (heap) vs {right:?} (arena)"
                ),
            }
        }
    }
}
