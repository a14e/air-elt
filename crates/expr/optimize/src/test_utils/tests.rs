//! Optimizer tests: golden structural rewrites, ground-truth evaluation, and a
//! property test asserting optimization preserves a program's meaning.

#![allow(clippy::unwrap_used)]

use std::path::PathBuf;
use std::sync::Arc;

use air_elt_expr_funcs::FuncError;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
use air_elt_expr_parse::model::{Expr, LiteralValue};
use air_elt_expr_parse::{Parser, Program};
use air_elt_expr_types::limits::MAX_EXPR_DEPTH;
use air_elt_types::{DataType, Field, Key, Schema, Value};

use crate::ExpectedOutput;
use proptest::prelude::*;

use crate::engines::type_check::{TypeChecker, TypeMap};
use crate::engines::{Compactor, EvalError, FieldSource, ProgramEvaluator};
use crate::error::OptimizeError;
use crate::model::node_id::{NodeCounter, NodeId};
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::opt_program::{OptProgram, OptStatement};
use crate::model::{CompactProgram, NodeRef, OptNode, RegisterId, TypeClass};
use crate::optimizer::Optimizer;
use crate::pass::Pass;
use crate::rules::{RewriteDriver, RuleSet};

// Thin wrappers exercising the OOP API once, so the test bodies stay terse.

fn optimize(
    program: &Program,
    registry: &FunctionRegistry,
    context: &EvalContext,
    apply_rules: bool,
) -> Result<OptProgram, OptimizeError> {
    Optimizer::create(registry, context).optimize(program, apply_rules, None)
}

fn compact(lowered: OptProgram) -> Result<CompactProgram, OptimizeError> {
    Compactor::create().compact(lowered)
}

fn eval_const_program(
    program: &CompactProgram,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Value, EvalError> {
    ProgramEvaluator::create(program, registry, context).evaluate()
}

struct NoEnv;
impl EnvResolver for NoEnv {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
}

struct NoFiles;
impl FileResolver for NoFiles {
    fn read(&self, path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
        Err(FuncError::FileReadFailed {
            path: path.to_owned(),
            reason: "not available in tests".to_owned(),
        })
    }
}

fn registry() -> FunctionRegistry {
    FunctionRegistry::with_builtins()
}

fn context() -> EvalContext {
    EvalContext {
        env_resolver: Arc::new(NoEnv),
        file_resolver: Arc::new(NoFiles),
        now: chrono::Utc::now(),
        base_dir: PathBuf::from("/tmp"),
        is_compile_time: true,
        caches: air_elt_expr_funcs::ExprCaches::default(),
    }
}

fn parse(source: &str) -> air_elt_expr_parse::Program {
    Parser::create().parse_expression(source).unwrap()
}

/// The result expression after full optimization.
fn optimized_result(source: &str) -> OptExpr {
    let registry = registry();
    let context = context();
    let program = optimize(&parse(source), &registry, &context, true).unwrap();
    program.result
}

fn compile_optimized(source: &str) -> CompactProgram {
    Optimizer::create(&registry(), &context())
        .compile(&parse(source), None, None)
        .unwrap()
}

fn compile_unoptimized(source: &str) -> CompactProgram {
    let registry = registry();
    let context = context();
    let lowered = optimize(&parse(source), &registry, &context, false).unwrap();
    compact(lowered).unwrap()
}

fn eval_unoptimized(source: &str) -> Value {
    let registry = registry();
    let context = context();
    eval_const_program(&compile_unoptimized(source), &registry, &context).unwrap()
}

/// Run the bottom-up rewrite fixpoint and the one-shot finalizers over a
/// hand-built result expression. For rules whose trigger shape cannot arise from
/// source — a nested `Switch` (`flatten_conditionals` would merge a conditional
/// chain first) or a register-keyed membership `Or` (a register pinned to a
/// constant is inlined before it) — this exercises just the rewrite stage of
/// [`Optimizer::optimize`].
fn rewrite_result(result: OptExpr, register_count: RegisterId) -> OptExpr {
    let registry = registry();
    let context = context();
    let rule_set = RuleSet::create(&registry);
    let node_counter = NodeCounter::create();
    let driver = RewriteDriver::create(&rule_set, &registry, &context, &node_counter);
    let mut program = OptProgram {
        statements: vec![],
        result,
        register_count,
    };
    driver.optimize(&mut program);
    driver.finalize(&mut program);
    program.result
}

/// `equals(Register(register), constant)` — a single membership clause.
fn register_equals(register: RegisterId, constant: Value) -> OptExpr {
    let equals = registry().get_ref("equals", Some(2)).unwrap();
    OptExpr::Call {
        id: NodeId::PLACEHOLDER,
        func: equals,
        args: vec![
            OptExpr::Register(NodeId::PLACEHOLDER, register),
            OptExpr::Const(NodeId::PLACEHOLDER, constant),
        ],
    }
}

/// Evaluate a hand-built result expression with register 0 pinned to `seed`.
fn eval_with_register0(result: OptExpr, seed: Value) -> Value {
    let program = OptProgram {
        statements: vec![OptStatement {
            register: 0,
            value: OptExpr::Const(NodeId::PLACEHOLDER, seed),
        }],
        result,
        register_count: 1,
    };
    let compiled = compact(program).unwrap();
    eval_const_program(&compiled, &registry(), &context()).unwrap()
}

/// Borrow an object literal's fields, panicking if `value` is not a `Value::Object`.
fn object_entries(value: &Value) -> &[(String, Value)] {
    match value {
        Value::Object(entries) => entries,
        other => panic!("expected Value::Object, got {other:?}"),
    }
}

/// The first value bound to `key` in an object literal.
fn object_field<'a>(value: &'a Value, key: &str) -> &'a Value {
    object_entries(value)
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, val)| val)
        .unwrap_or_else(|| panic!("key {key:?} not found in {value:?}"))
}

// ---- Golden structural rewrites -------------------------------------------

// ---- Guard propagation (operand substitution) -----------------------------

#[test]
fn substitutes_null_operand_in_is_null_branch() {
    // In the `isNull(x)` branch `x` is null, so `upper(x)` folds to null; the
    // else branch only knows `x` is non-null, so the read stays.
    let result = optimized_result(r#"if(isNull(field("c")), upper(field("c")), field("c"))"#);
    let OptExpr::If {
        then_branch,
        else_branch,
        ..
    } = result
    else {
        panic!("expected if, got {result:?}");
    };
    assert_eq!(
        *then_branch,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Null)
    );
    // The else only knows `x` is non-null (no value), so the read survives —
    // here a hoisted register, since the field is read several times.
    assert!(
        matches!(
            *else_branch,
            OptExpr::Register(..) | OptExpr::SourceField(..)
        ),
        "expected the operand read to survive, got {else_branch:?}"
    );
}

#[test]
fn if_null_alternative_folds_through_null_to_the_value() {
    // `ifNull(c, upper(c))`: the alternative runs only when `c` is null, so guard
    // propagation threads that fact in and `upper(c)` folds to null; the resulting
    // `ifNull(c, null)` then collapses to `c`.
    assert_eq!(
        optimized_result(r#"ifNull(field("c"), upper(field("c")))"#),
        OptExpr::SourceField(NodeId::PLACEHOLDER, "c".to_string())
    );
}

#[test]
fn if_null_with_null_alternative_folds_to_value() {
    // `ifNull(value, null)` ≡ `value` — the redundant wrapper is dropped.
    assert_eq!(
        optimized_result(r#"ifNull(field("c"), null)"#),
        OptExpr::SourceField(NodeId::PLACEHOLDER, "c".to_string())
    );
}

#[test]
fn substitutes_string_equality_operand_in_then_branch() {
    // `x == "yes"` pins `x` to the string, so `upper(x)` folds to "YES".
    let result = optimized_result(r#"if(field("c") == "yes", upper(field("c")), field("c"))"#);
    let OptExpr::If { then_branch, .. } = result else {
        panic!("expected if, got {result:?}");
    };
    assert_eq!(
        *then_branch,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("YES".to_string()))
    );
}

#[test]
fn keeps_numeric_equality_operand_unsubstituted() {
    // A numeric equality pins the value but not its width, so the operand is NOT
    // substituted (it would diverge `typeof`); the call is left for the typed pass.
    let result = optimized_result(r#"if(field("c") == 5, typeof(field("c")), field("c"))"#);
    let OptExpr::If { then_branch, .. } = result else {
        panic!("expected if, got {result:?}");
    };
    assert!(
        matches!(*then_branch, OptExpr::Call { .. }),
        "expected an unfolded call, got {then_branch:?}"
    );
}

#[test]
fn threads_equality_fact_into_and_right_operand() {
    // `x == "a"` proves `x` is "a" in the right operand, where `x == "b"` folds to
    // false — a conflicting-equality conjunction (the `&&` itself stays, since the
    // left operand is non-constant).
    let result = optimized_result(r#"field("c") == "a" && field("c") == "b""#);
    let OptExpr::And { right, .. } = result else {
        panic!("expected and, got {result:?}");
    };
    assert_eq!(
        *right,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(false))
    );
}

#[test]
fn substitutes_null_in_is_not_null_else_branch() {
    // `isNotNull(x)` false ⇒ `x` is null, so the ELSE branch's `upper(x)` folds to
    // null — the symmetric, negative-truth counterpart of the `isNull` then-branch.
    let result = optimized_result(r#"if(isNotNull(field("c")), field("c"), upper(field("c")))"#);
    let OptExpr::If { else_branch, .. } = result else {
        panic!("expected if, got {result:?}");
    };
    assert_eq!(
        *else_branch,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Null)
    );
}

#[test]
fn folds_not_equals_fact_in_else_branch() {
    // `x != "yes"` false ⇒ `x == "yes"`, so the ELSE branch pins `x` to "yes" and
    // `upper(x)` folds to "YES" — the `notEquals` guard on the negative path.
    let result = optimized_result(r#"if(field("c") != "yes", field("c"), upper(field("c")))"#);
    let OptExpr::If { else_branch, .. } = result else {
        panic!("expected if, got {result:?}");
    };
    assert_eq!(
        *else_branch,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("YES".to_string()))
    );
}

#[test]
fn rejects_conjunction_asserting_incompatible_types() {
    // `contains(x,"")` asserts `x` is a string; `!(!x)` asserts it is a bool. A
    // `&&` of the two raises for every non-null row, so it is a compile error.
    let registry = registry();
    let context = context();
    let program = parse(r#"contains(field("c"), "") && !(!(field("c")))"#);
    let result = optimize(&program, &registry, &context, true);
    assert!(
        matches!(result, Err(OptimizeError::InfeasibleConjunction { .. })),
        "expected InfeasibleConjunction, got {result:?}"
    );
}

#[test]
fn accepts_feasible_conjunctions() {
    let registry = registry();
    let context = context();
    // Same operand asserted to the SAME class twice — no contradiction.
    let same_class = parse(r#"contains(field("c"), "") && startsWith(field("c"), "")"#);
    assert!(optimize(&same_class, &registry, &context, true).is_ok());
    // Disjoint classes but on DIFFERENT operands — feasible (guards that the
    // operand key actually discriminates `c` from `d`).
    let different_operands = parse(r#"contains(field("c"), "") && !(!(field("d")))"#);
    assert!(optimize(&different_operands, &registry, &context, true).is_ok());
}

#[test]
fn folds_arithmetic_to_constant() {
    assert_eq!(
        optimized_result("1 + 2"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(3))
    );
    assert_eq!(
        optimized_result("2 * 3 + 4"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(10))
    );
}

#[test]
fn folds_nested_pure_calls() {
    assert_eq!(
        optimized_result("concat(toString(1 + 2), '!')"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("3!".to_string()))
    );
}

#[test]
fn duration_literal_folds_to_interval_constant() {
    use std::time::Duration;
    // Compact human and ISO-8601 forms both lower to a constant interval.
    assert_eq!(
        optimized_result("10s"),
        OptExpr::Const(
            NodeId::PLACEHOLDER,
            Value::Interval(Duration::from_secs(10))
        )
    );
    assert_eq!(
        optimized_result("1h30m"),
        OptExpr::Const(
            NodeId::PLACEHOLDER,
            Value::Interval(Duration::from_secs(5400))
        )
    );
    assert_eq!(
        eval_unoptimized("PT1H30M"),
        Value::Interval(Duration::from_secs(5400))
    );
}

#[test]
fn collapses_field_forms_to_source_field() {
    let expected = OptExpr::SourceField(NodeId::PLACEHOLDER, "x".to_string());
    assert_eq!(optimized_result("field(\"x\")"), expected);
    assert_eq!(optimized_result("field(`x`)"), expected);
    assert_eq!(optimized_result("field(field(\"x\"))"), expected);
}

#[test]
fn double_negation_collapses_to_bool_type_assert() {
    // not(not(x)) is a Bool involution → TypeAssert{Bool, Identity}, which keeps
    // the type/null check the outer `not` performed (bare `x` would drop it).
    match optimized_result("!(!(field(\"flag\")))") {
        OptExpr::TypeAssert {
            inner,
            expect,
            on_present,
            ..
        } => {
            assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "flag"));
            assert_eq!(expect, TypeClass::Bool);
            assert_eq!(on_present, AssertYield::Identity);
        }
        other => panic!("expected a Bool TypeAssert, got {other:?}"),
    }
}

#[test]
fn prunes_multi_if_with_constant_conditions() {
    assert_eq!(
        optimized_result("multiIf(false, 1, true, 2, 3)"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(2))
    );
}

#[test]
fn prunes_constant_and_or() {
    // `true && x` keeps the evaluator's Bool/Null requirement on the surviving
    // operand: `field("b")` has no provable type, so the fold wraps it in a
    // TypeAssert{Bool} instead of yielding it bare (which would erase the
    // runtime type error the unoptimized path raises for a non-bool field).
    assert_eq!(
        optimized_result("true && field(\"b\")"),
        OptExpr::TypeAssert {
            id: NodeId::PLACEHOLDER,
            inner: Box::new(OptExpr::SourceField(NodeId::PLACEHOLDER, "b".to_string())),
            expect: TypeClass::Bool,
            on_present: AssertYield::Identity,
        }
    );
    assert_eq!(
        optimized_result("false && field(\"b\")"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(false))
    );
    assert_eq!(
        optimized_result("true || field(\"b\")"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(true))
    );
}

#[test]
fn folds_interpolation_and_object() {
    let registry = registry();
    let context = context();
    let interpolation = Parser::create().parse("a{1 + 1}b").unwrap();
    let folded = optimize(&interpolation, &registry, &context, true)
        .unwrap()
        .result;
    assert_eq!(
        folded,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a2b".to_string()))
    );

    assert_eq!(
        optimized_result("{\"k\" = 1 + 1}"),
        OptExpr::Const(
            NodeId::PLACEHOLDER,
            Value::Object(vec![("k".to_string(), Value::Int64(2))])
        ),
    );
}

#[test]
fn inlines_constant_statement_and_drops_register() {
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse("x = 1 + 1; x * field(\"n\")"),
        &registry,
        &context,
        true,
    )
    .unwrap();
    // The constant binding `x` is inlined and removed.
    assert!(program.statements.is_empty());
    // `x` became the constant 2 inside the surviving multiplication.
    assert!(matches!(program.result, OptExpr::Call { .. }));
}

#[test]
fn prunes_statement_whose_register_is_unused() {
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse("x = field(\"a\"); field(\"a\") + 1"),
        &registry,
        &context,
        true,
    )
    .unwrap();
    assert!(program.statements.is_empty());
}

#[test]
fn keeps_unread_binding_whose_value_can_fail() {
    // `x` is never read, but `divide` may fail (division by zero). Statements
    // are evaluated eagerly, so the binding must survive to preserve the error.
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse("x = field(\"a\") / field(\"b\"); field(\"c\")"),
        &registry,
        &context,
        true,
    )
    .unwrap();
    assert_eq!(program.statements.len(), 1);
}

#[test]
fn prunes_unread_binding_whose_value_is_infallible() {
    // `add` cannot fail for well-typed arguments, so an unread `add` binding is
    // safe to drop entirely.
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse("x = field(\"a\") + field(\"b\"); field(\"c\")"),
        &registry,
        &context,
        true,
    )
    .unwrap();
    assert!(program.statements.is_empty());
}

#[test]
fn rejects_constant_evaluation_error_at_compile_time() {
    // A constant that fails to evaluate in an eager position stops the build
    // rather than deferring the error to runtime.
    let registry = registry();
    let context = context();
    let program = parse("x = 1 / 0; 5");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn keeps_constant_error_in_a_dead_branch() {
    // The erroring `1 / 0` is in an unreachable branch (`if(false, …)`), so the
    // optimizer must NOT fail the build — dce drops the dead branch.
    let registry = registry();
    let context = context();
    let program = parse("if(1 < 0, 1 / 0, 7)");
    let compiled = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(compiled.is_ok());
}

#[test]
fn keeps_constant_error_in_a_dead_multi_if_branch() {
    // `1 / 0` is a conditional branch value guarded by a non-constant condition:
    // it is reached only when the condition holds, so the error is lazy. The
    // conditional survives (no const-fold of the dead arm, no build failure); the
    // single-branch multiIf collapses to an `if` in finalization, with the lazy
    // `1 / 0` preserved unfolded in the then-branch.
    match optimized_result("multiIf(field(\"x\") == 1, 1 / 0, 9)") {
        OptExpr::If { then_branch, .. } => {
            assert!(
                matches!(*then_branch, OptExpr::Call { .. }),
                "the lazy 1/0 stays an unfolded call"
            );
        }
        other => panic!("expected a surviving if, got {other:?}"),
    }
}

#[test]
fn rejects_invalid_constant_regex_in_eager_position() {
    // An invalid constant regex pattern fails to compile during const folding;
    // in an always-evaluated position the optimizer reports it eagerly.
    let registry = registry();
    let context = context();
    let program = parse("regexMatch(\"abc\", \"[\")");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn keeps_invalid_constant_regex_in_a_dead_branch() {
    // The same invalid regex inside an unreachable branch must NOT fail the
    // build — dce drops the branch before it can reach runtime.
    let registry = registry();
    let context = context();
    let program = parse("if(1 < 0, regexMatch(\"abc\", \"[\"), false)");
    let compiled = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(compiled.is_ok());
}

#[test]
fn defers_division_by_zero_with_dynamic_operand_to_runtime() {
    // `field("a") / 0` has a non-constant operand, so it cannot be folded: the
    // program compiles cleanly and the division error surfaces only at runtime.
    let registry = registry();
    let context = context();
    let program = parse("x = field(\"a\") / 0; x");
    let compiled = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(compiled.is_ok());
}

#[test]
fn rejects_invalid_const_regex_with_dynamic_text() {
    // The pattern is a constant but the subject text is dynamic, so the eager
    // const check cannot fire (not all-const). `validate_const_args` still
    // catches the malformed inlined pattern → `InvalidConstArg`.
    let registry = registry();
    let context = context();
    let program = parse("regexMatch(field(\"x\"), \"(\")");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::InvalidConstArg { .. })));
}

#[test]
fn rejects_invalid_const_jspath_with_dynamic_json() {
    let registry = registry();
    let context = context();
    let program = parse("jsPath(field(\"doc\"), \"$[\")");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::InvalidConstArg { .. })));
}

#[test]
fn rejects_invalid_const_regex_even_in_a_lazy_branch() {
    // Unlike a value-failure (`1 / 0`), a malformed inlined format literal can
    // never be valid, so it is reported in EVERY position — including a lazy
    // conditional branch that the eager const check would skip.
    let registry = registry();
    let context = context();
    let program = parse("if(field(\"c\"), regexMatch(field(\"x\"), \"(\"), false)");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::InvalidConstArg { .. })));
}

#[test]
fn rejects_non_const_field_argument() {
    // `field(<dynamic>)` has no statically-known column name. `field("x")` and
    // the backtick form collapse to a resolved column; a surviving `Field`
    // (here the name is `upper(field("x"))`) is a compile error.
    let registry = registry();
    let context = context();
    let program = parse("field(upper(field(\"x\")))");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::NonConstFieldArg)));
}

#[test]
fn defers_value_failure_in_lazy_and_right_operand() {
    // `&&` right operand is lazy: a dynamic left keeps the `&&` unfolded, so the
    // constant `1 / 0` on the right defers to runtime — compilation succeeds.
    let registry = registry();
    let context = context();
    let program = parse("field(\"c\") && ((1 / 0) == 0)");
    let compiled = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(compiled.is_ok());
}

#[test]
fn rejects_value_failure_in_eager_and_left_operand() {
    // `&&` left operand is always evaluated → the constant `1 / 0` fails the build.
    let registry = registry();
    let context = context();
    let program = parse("((1 / 0) == 0) && field(\"c\")");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn defers_value_failure_in_if_null_alternative() {
    // The `ifNull` alternative is lazy (reached only when the value is null).
    let registry = registry();
    let context = context();
    let program = parse("ifNull(field(\"x\"), 1 / 0)");
    let compiled = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(compiled.is_ok());
}

#[test]
fn rejects_value_failure_in_null_if_operand() {
    // `nullIf` evaluates both operands unconditionally → eager → fails the build.
    let registry = registry();
    let context = context();
    let program = parse("nullIf(field(\"x\"), 1 / 0)");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn keeps_constant_error_in_a_dead_switch_arm() {
    // A >5-branch equality multiIf lowers to a Switch; one arm computes `1 / 0`,
    // reached only when its key matches. The arm value is a lazy position, so
    // the optimizer leaves the error for runtime instead of failing the build.
    let source = "multiIf(\
        field(\"x\") == 1, 1 / 0, field(\"x\") == 2, 20, field(\"x\") == 3, 30, \
        field(\"x\") == 4, 40, field(\"x\") == 5, 50, field(\"x\") == 6, 60, 0)";
    match optimized_result(source) {
        OptExpr::Switch { table, .. } => assert_eq!(table.len(), 6),
        other => panic!("expected a Switch with a lazy erroring arm, got {other:?}"),
    }
}

// ---- Finalize: multiIf → if collapse --------------------------------------

/// The source-field name an `equals(field, const)` condition tests, if shaped so.
fn equals_field_name(condition: &OptExpr) -> Option<&str> {
    let OptExpr::Call { args, .. } = condition else {
        return None;
    };
    match args.first() {
        Some(OptExpr::SourceField(_, name)) => Some(name.as_str()),
        _ => None,
    }
}

#[test]
fn collapses_single_branch_multi_if_to_if() {
    // A one-branch multiIf that did not switch-lower becomes a plain if; the
    // default becomes the else branch.
    match optimized_result("multiIf(field(\"a\") == 1, 10, 0)") {
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            assert_eq!(equals_field_name(&condition), Some("a"));
            assert_eq!(
                *then_branch,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(10)),
                "branch value"
            );
            assert_eq!(
                *else_branch,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(0)),
                "default → else"
            );
        }
        other => panic!("expected an if, got {other:?}"),
    }
}

#[test]
fn collapses_two_branch_multi_if_to_nested_if() {
    // Two branches over distinct fields collapse to if(a==1, .., if(b==2, ..,
    // default)) — branch order preserved, default as the innermost else.
    match optimized_result("multiIf(field(\"a\") == 1, 10, field(\"b\") == 2, 20, 0)") {
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            assert_eq!(equals_field_name(&condition), Some("a"));
            assert_eq!(
                *then_branch,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(10)),
                "first value"
            );
            match *else_branch {
                OptExpr::If {
                    condition: inner_condition,
                    then_branch: inner_then,
                    else_branch: inner_else,
                    ..
                } => {
                    assert_eq!(equals_field_name(&inner_condition), Some("b"));
                    assert_eq!(
                        *inner_then,
                        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(20)),
                        "second value"
                    );
                    assert_eq!(
                        *inner_else,
                        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(0)),
                        "default → innermost else"
                    );
                }
                other => panic!("expected a nested if, got {other:?}"),
            }
        }
        other => panic!("expected an outer if, got {other:?}"),
    }
}

#[test]
fn if_chain_collapses_back_without_oscillating() {
    // flatten_conditionals canonicalizes the if-chain into a multiIf inside the
    // fixpoint; the finalizer collapses it back. The pipeline must terminate on
    // the nested-if form rather than oscillate between the two shapes.
    match optimized_result("if(field(\"a\") == 1, 10, if(field(\"b\") == 2, 20, 0))") {
        OptExpr::If {
            condition,
            else_branch,
            ..
        } => {
            assert_eq!(equals_field_name(&condition), Some("a"));
            assert!(
                matches!(*else_branch, OptExpr::If { .. }),
                "lands on a nested if"
            );
        }
        other => panic!("expected a nested if, got {other:?}"),
    }
}

#[test]
fn collapses_or_keyed_two_branch_multi_if() {
    // A ≤2-branch multiIf is never a switch candidate (switch lowering needs > 5
    // branches), so an or-keyed condition collapses to a plain if like any other
    // — the `Or` is kept intact in the if condition.
    let source = "multiIf(field(\"a\") == 1 || field(\"b\") == 2, 10, field(\"c\") == 3, 20, 0)";
    match optimized_result(source) {
        OptExpr::If { condition, .. } => {
            assert!(
                matches!(*condition, OptExpr::Or { .. }),
                "the or condition is preserved in the if"
            );
        }
        other => panic!("expected a collapsed if, got {other:?}"),
    }
}

#[test]
fn keeps_three_branch_multi_if() {
    // Above the collapse threshold (and below the switch threshold) the flat
    // multiIf form is kept.
    let source = "multiIf(field(\"a\") == 1, 10, field(\"b\") == 2, 20, field(\"c\") == 3, 30, 0)";
    match optimized_result(source) {
        OptExpr::MultiIf { branches, .. } => assert_eq!(branches.len(), 3),
        other => panic!("expected a multiIf, got {other:?}"),
    }
}

// ---- De Morgan: factoring two negations into one --------------------------

#[test]
fn factors_conjunction_of_negations() {
    // not(a) && not(b) → not(a || b): the two `not`s merge into one.
    let not = registry().get_ref("not", Some(1)).unwrap();
    let expected = OptExpr::Call {
        id: NodeId::PLACEHOLDER,
        func: not,
        args: vec![OptExpr::Or {
            id: NodeId::PLACEHOLDER,
            left: Box::new(OptExpr::SourceField(NodeId::PLACEHOLDER, "a".to_string())),
            right: Box::new(OptExpr::SourceField(NodeId::PLACEHOLDER, "b".to_string())),
        }],
    };
    assert_eq!(optimized_result("!field(\"a\") && !field(\"b\")"), expected);
}

#[test]
fn factors_disjunction_of_negations() {
    // not(a) || not(b) → not(a && b).
    let not = registry().get_ref("not", Some(1)).unwrap();
    let expected = OptExpr::Call {
        id: NodeId::PLACEHOLDER,
        func: not,
        args: vec![OptExpr::And {
            id: NodeId::PLACEHOLDER,
            left: Box::new(OptExpr::SourceField(NodeId::PLACEHOLDER, "a".to_string())),
            right: Box::new(OptExpr::SourceField(NodeId::PLACEHOLDER, "b".to_string())),
        }],
    };
    assert_eq!(optimized_result("!field(\"a\") || !field(\"b\")"), expected);
}

#[test]
fn de_morgan_composes_with_double_negation_collapse() {
    // !(!a || !b) → not(not(a && b)) → TypeAssert{Bool, Identity} over (a && b):
    // De Morgan factors the inner disjunction, then the not-involution round-trip
    // collapses the doubled negation.
    match optimized_result("!(!field(\"a\") || !field(\"b\"))") {
        OptExpr::TypeAssert {
            inner,
            expect,
            on_present,
            ..
        } => {
            assert!(matches!(*inner, OptExpr::And { .. }));
            assert_eq!(expect, TypeClass::Bool);
            assert_eq!(on_present, AssertYield::Identity);
        }
        other => panic!("expected a TypeAssert over the And, got {other:?}"),
    }
}

#[test]
fn keeps_mixed_negation_unfactored() {
    // Only the left operand is negated → De Morgan does not apply; the `&&` stays.
    match optimized_result("!field(\"a\") && field(\"b\")") {
        OptExpr::And { right, .. } => {
            assert!(matches!(*right, OptExpr::SourceField(_, name) if name == "b"));
        }
        other => panic!("expected a kept And, got {other:?}"),
    }
}

#[test]
fn keeps_single_negation_in_disjunction_unfactored() {
    // Dual no-fire case: only the right operand is negated, on the `Or` arm — De
    // Morgan does not apply and the `||` is kept intact.
    match optimized_result("field(\"a\") || !field(\"b\")") {
        OptExpr::Or { left, .. } => {
            assert!(matches!(*left, OptExpr::SourceField(_, name) if name == "a"));
        }
        other => panic!("expected a kept Or, got {other:?}"),
    }
}

// ---- TypeAssert: empty-needle + round-trip collapse -----------------------

#[test]
fn collapses_empty_needle_predicates_to_type_assert() {
    // contains/startsWith/endsWith(x, "") → TypeAssert{String, Const(true)}.
    for predicate in ["contains", "startsWith", "endsWith"] {
        let source = format!("{predicate}(field(\"x\"), \"\")");
        match optimized_result(&source) {
            OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
                ..
            } => {
                assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
                assert_eq!(expect, TypeClass::String);
                assert_eq!(on_present, AssertYield::Const(Value::Bool(true)));
            }
            other => panic!("{predicate}: expected a TypeAssert, got {other:?}"),
        }
    }
}

#[test]
fn collapses_double_reverse_to_identity_type_assert() {
    // reverse(reverse(x)) → TypeAssert{String, Identity}: reverse is a total
    // involution on strings, so only the operand's type/null check remains.
    match optimized_result("reverse(reverse(field(\"x\")))") {
        OptExpr::TypeAssert {
            inner,
            expect,
            on_present,
            ..
        } => {
            assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
            assert_eq!(expect, TypeClass::String);
            assert_eq!(on_present, AssertYield::Identity);
        }
        other => panic!("expected a TypeAssert, got {other:?}"),
    }
}

#[test]
fn keeps_non_empty_needle_and_single_reverse() {
    // A non-empty needle is not unconditionally true; a single reverse is not a
    // round-trip. Both keep their calls.
    assert!(matches!(
        optimized_result("contains(field(\"x\"), \"a\")"),
        OptExpr::Call { .. }
    ));
    assert!(matches!(
        optimized_result("reverse(field(\"x\"))"),
        OptExpr::Call { .. }
    ));
    // Inner is a single-arg call but not the matching inverse → table miss, kept.
    assert!(matches!(
        optimized_result("reverse(upper(field(\"x\")))"),
        OptExpr::Call { .. }
    ));
    // Both outer and inner are table members, but not a MATCHING pair
    // (bytesFromHex pairs with hex, not base64) → no collapse.
    assert!(matches!(
        optimized_result("bytesFromHex(base64(field(\"x\")))"),
        OptExpr::Call { .. }
    ));
}

// ---- concat algebra (strict concat) ---------------------------------------

#[test]
fn concat_with_empty_string_becomes_string_assert() {
    // concat(x, "") and concat("", x) → TypeAssert{String, Identity}: strict
    // concat requires `x` to be a string, and appending "" is a no-op.
    for source in [r#"concat(field("x"), "")"#, r#"concat("", field("x"))"#] {
        match optimized_result(source) {
            OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
                ..
            } => {
                assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
                assert_eq!(expect, TypeClass::String);
                assert_eq!(on_present, AssertYield::Identity);
            }
            other => panic!("{source}: expected a TypeAssert, got {other:?}"),
        }
    }
}

#[test]
fn single_argument_concat_becomes_string_assert() {
    // concat(x) of one dynamic operand is a bare string type-check.
    match optimized_result(r#"concat(field("x"))"#) {
        OptExpr::TypeAssert {
            inner,
            expect,
            on_present,
            ..
        } => {
            assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
            assert_eq!(expect, TypeClass::String);
            assert_eq!(on_present, AssertYield::Identity);
        }
        other => panic!("expected a TypeAssert, got {other:?}"),
    }
}

#[test]
fn concat_drops_empty_string_arguments() {
    // concat(a, "", b) → concat(a, b): empty constants contribute nothing.
    match optimized_result(r#"concat(field("a"), "", field("b"))"#) {
        OptExpr::Call { args, .. } => {
            assert_eq!(args.len(), 2);
            assert!(matches!(&args[0], OptExpr::SourceField(_, name) if name == "a"));
            assert!(matches!(&args[1], OptExpr::SourceField(_, name) if name == "b"));
        }
        other => panic!("expected a two-arg concat, got {other:?}"),
    }
}

#[test]
fn concat_of_only_empty_strings_folds_to_empty() {
    assert_eq!(
        optimized_result(r#"concat("", "")"#),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text(String::new()))
    );
}

#[test]
fn trim_of_concat_strips_whitespace_constant_edges() {
    // trim(concat("  ", x, "  ")) drops the whitespace-only edges; the inner
    // concat then collapses to a single string assert, with `trim` kept.
    match optimized_result(r#"trim(concat("  ", field("x"), "  "))"#) {
        OptExpr::Call { args, .. } => {
            assert_eq!(args.len(), 1);
            match &args[0] {
                OptExpr::TypeAssert { inner, expect, .. } => {
                    assert!(matches!(&**inner, OptExpr::SourceField(_, name) if name == "x"));
                    assert_eq!(*expect, TypeClass::String);
                }
                other => panic!("expected a string assert under trim, got {other:?}"),
            }
        }
        other => panic!("expected trim(...), got {other:?}"),
    }
}

#[test]
fn trim_of_concat_left_trims_partial_whitespace_edge() {
    // A partially-whitespace leading constant keeps its non-whitespace tail.
    match optimized_result(r#"trim(concat("  hi", field("x")))"#) {
        OptExpr::Call { args, .. } => {
            assert_eq!(args.len(), 1);
            match &args[0] {
                OptExpr::Call {
                    args: concat_args, ..
                } => {
                    assert_eq!(concat_args.len(), 2);
                    assert_eq!(
                        concat_args[0],
                        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("hi".into()))
                    );
                    assert!(
                        matches!(&concat_args[1], OptExpr::SourceField(_, name) if name == "x")
                    );
                }
                other => panic!("expected inner concat, got {other:?}"),
            }
        }
        other => panic!("expected trim(...), got {other:?}"),
    }
}

// ---- encode/decode label-matched round-trip -------------------------------

#[test]
fn decode_of_encode_same_label_collapses_to_bytes_assert() {
    for algorithm in ["hex", "base64", "base64url"] {
        let source = format!(r#"decode(encode(field("x"), "{algorithm}"), "{algorithm}")"#);
        match optimized_result(&source) {
            OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
                ..
            } => {
                assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
                assert_eq!(expect, TypeClass::Bytes);
                assert_eq!(on_present, AssertYield::Identity);
            }
            other => panic!("{algorithm}: expected a Bytes assert, got {other:?}"),
        }
    }
}

#[test]
fn decode_of_encode_mismatched_or_reversed_is_kept() {
    // Different algorithms do not round-trip.
    assert!(matches!(
        optimized_result(r#"decode(encode(field("x"), "hex"), "base64")"#),
        OptExpr::Call { .. }
    ));
    // encode(decode(...)) is NOT collapsed — `decode` can fail on malformed text,
    // so that error must be preserved.
    assert!(matches!(
        optimized_result(r#"encode(decode(field("x"), "hex"), "hex")"#),
        OptExpr::Call { .. }
    ));
}

// ---- nested TypeAssert collapse -------------------------------------------

#[test]
fn nested_type_asserts_collapse_to_one() {
    // concat(reverse(reverse(x)), "") stacks a String assert inside a String
    // assert; the nested-assert collapse flattens them to one.
    match optimized_result(r#"concat(reverse(reverse(field("x"))), "")"#) {
        OptExpr::TypeAssert {
            inner,
            expect,
            on_present,
            ..
        } => {
            assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
            assert_eq!(expect, TypeClass::String);
            assert_eq!(on_present, AssertYield::Identity);
        }
        other => panic!("expected a single TypeAssert, got {other:?}"),
    }
}

#[test]
fn type_assert_with_const_inner_yield_does_not_collapse() {
    // A `Const` inner yield is not provably of the outer class, so the outer
    // assert over it is NOT redundant — the nesting is kept.
    let inner = OptExpr::TypeAssert {
        id: NodeId::PLACEHOLDER,
        inner: Box::new(OptExpr::Register(NodeId::PLACEHOLDER, 0)),
        expect: TypeClass::String,
        on_present: AssertYield::Const(Value::Bool(true)),
    };
    let outer = OptExpr::TypeAssert {
        id: NodeId::PLACEHOLDER,
        inner: Box::new(inner),
        expect: TypeClass::String,
        on_present: AssertYield::Identity,
    };
    match rewrite_result(outer, 1) {
        OptExpr::TypeAssert { inner, .. } => assert!(matches!(*inner, OptExpr::TypeAssert { .. })),
        other => panic!("expected kept nesting, got {other:?}"),
    }
}

// ---- OR-of-equals → membership Switch -------------------------------------

#[test]
fn long_or_of_equals_lowers_to_membership_switch() {
    // k==1 || k==2 || ... || k==6 → Switch{1..6 → true, default false}.
    let mut source = String::from(r#"field("k") == 1"#);
    for index in 2..=6 {
        source.push_str(&format!(r#" || field("k") == {index}"#));
    }
    match optimized_result(&source) {
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            assert_eq!(inputs.len(), 1);
            assert!(matches!(&inputs[0], OptExpr::SourceField(_, name) if name == "k"));
            assert_eq!(table.len(), 6);
            assert!(
                table
                    .iter()
                    .all(|(_, value)| *value
                        == OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(true)))
            );
            assert_eq!(
                *default,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(false))
            );
        }
        other => panic!("expected a membership Switch, got {other:?}"),
    }
}

#[test]
fn short_or_of_equals_stays_disjunction() {
    let mut source = String::from(r#"field("k") == 1"#);
    for index in 2..=5 {
        source.push_str(&format!(r#" || field("k") == {index}"#));
    }
    assert!(matches!(optimized_result(&source), OptExpr::Or { .. }));
}

#[test]
fn or_of_equals_below_distinct_threshold_after_dedup_stays_disjunction() {
    // Six clauses but only five DISTINCT keys (`k==1` twice) — after dedup the
    // membership table is below threshold, so it stays an `Or`.
    let mut source = String::from(r#"field("k") == 1"#);
    for index in [1, 2, 3, 4, 5] {
        source.push_str(&format!(r#" || field("k") == {index}"#));
    }
    assert!(matches!(optimized_result(&source), OptExpr::Or { .. }));
}

#[test]
fn membership_switch_evaluates_like_the_disjunction() {
    let mut chain = register_equals(0, Value::Int64(1));
    for index in 2..=6i64 {
        chain = OptExpr::Or {
            id: NodeId::PLACEHOLDER,
            left: Box::new(chain),
            right: Box::new(register_equals(0, Value::Int64(index))),
        };
    }
    let switch = rewrite_result(chain, 1);
    assert!(matches!(switch, OptExpr::Switch { .. }));
    assert_eq!(
        eval_with_register0(switch.clone(), Value::Int64(3)),
        Value::Bool(true)
    );
    assert_eq!(
        eval_with_register0(switch, Value::Int64(9)),
        Value::Bool(false)
    );
}

// ---- nested Switch collapse -----------------------------------------------

/// A `Switch` over `Register(0)` keyed on `Int64` constants.
fn int_switch(entries: Vec<(i64, &str)>, default: &str) -> OptExpr {
    OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Register(NodeId::PLACEHOLDER, 0)],
        table: entries
            .into_iter()
            .map(|(key, value)| {
                (
                    Key::from_value(&Value::Int64(key)).unwrap(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Text(value.to_owned())),
                )
            })
            .collect(),
        default: Box::new(OptExpr::Const(
            NodeId::PLACEHOLDER,
            Value::Text(default.to_owned()),
        )),
    }
}

#[test]
fn nested_switch_over_same_key_collapses() {
    // Switch{k, {1→a}, default: Switch{k, {1→x, 2→b}, default d}}
    //   → Switch{k, {1→a, 2→b}, default d}   (outer wins on key 1).
    let inner = int_switch(vec![(1, "x"), (2, "b")], "d");
    let outer = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Register(NodeId::PLACEHOLDER, 0)],
        table: vec![(
            Key::from_value(&Value::Int64(1)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into())),
        )],
        default: Box::new(inner),
    };
    match rewrite_result(outer, 1) {
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            assert_eq!(inputs, vec![OptExpr::Register(NodeId::PLACEHOLDER, 0)]);
            assert_eq!(table.len(), 2);
            let one = Key::from_value(&Value::Int64(1)).unwrap();
            let entry = table.iter().find(|(key, _)| *key == one).unwrap();
            // Outer wins on the shared key 1; the inner-only key 2 carries over.
            assert_eq!(
                entry.1,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into()))
            );
            let two = Key::from_value(&Value::Int64(2)).unwrap();
            let entry_two = table.iter().find(|(key, _)| *key == two).unwrap();
            assert_eq!(
                entry_two.1,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("b".into()))
            );
            assert_eq!(
                *default,
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("d".into()))
            );
        }
        other => panic!("expected a merged Switch, got {other:?}"),
    }
}

#[test]
fn nested_switch_over_different_key_is_kept() {
    let inner = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Register(NodeId::PLACEHOLDER, 1)],
        table: vec![(
            Key::from_value(&Value::Int64(2)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("b".into())),
        )],
        default: Box::new(OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("d".into()))),
    };
    let outer = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Register(NodeId::PLACEHOLDER, 0)],
        table: vec![(
            Key::from_value(&Value::Int64(1)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into())),
        )],
        default: Box::new(inner),
    };
    match rewrite_result(outer, 2) {
        OptExpr::Switch { default, .. } => assert!(matches!(*default, OptExpr::Switch { .. })),
        other => panic!("expected the outer Switch kept, got {other:?}"),
    }
}

#[test]
fn nested_switch_over_impure_key_is_kept() {
    // The two switches read the SAME key expression, but it is impure (random):
    // merging to one evaluation would change the result, so it must not collapse.
    let random_key = || {
        let random_int = registry().get_ref("randomInt", Some(2)).unwrap();
        OptExpr::Call {
            id: NodeId::PLACEHOLDER,
            func: random_int,
            args: vec![
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(0)),
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(10)),
            ],
        }
    };
    let inner = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![random_key()],
        table: vec![(
            Key::from_value(&Value::Int64(2)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("b".into())),
        )],
        default: Box::new(OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("d".into()))),
    };
    let outer = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![random_key()],
        table: vec![(
            Key::from_value(&Value::Int64(1)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into())),
        )],
        default: Box::new(inner),
    };
    match rewrite_result(outer, 0) {
        OptExpr::Switch { default, .. } => assert!(matches!(*default, OptExpr::Switch { .. })),
        other => panic!("expected the outer Switch kept, got {other:?}"),
    }
}

#[test]
fn switch_with_constant_key_folds_to_matched_branch() {
    let switch = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(2))],
        table: vec![
            (
                Key::from_value(&Value::Int64(1)).unwrap(),
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into())),
            ),
            (
                Key::from_value(&Value::Int64(2)).unwrap(),
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("b".into())),
            ),
            (
                Key::from_value(&Value::Int64(3)).unwrap(),
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("c".into())),
            ),
        ],
        default: Box::new(OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("z".into()))),
    };
    assert_eq!(
        rewrite_result(switch, 0),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("b".into()))
    );
}

#[test]
fn switch_with_constant_key_miss_folds_to_default() {
    let switch = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(9))],
        table: vec![(
            Key::from_value(&Value::Int64(1)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into())),
        )],
        default: Box::new(OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("z".into()))),
    };
    assert_eq!(
        rewrite_result(switch, 0),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("z".into()))
    );
}

#[test]
fn switch_on_inlined_constant_key_folds_through_the_pipeline() {
    // `x` is a constant binding used as the switch key: switch_lower builds the
    // Switch over the register, constant_inliner inlines `x = 3`, and BranchPrune
    // then folds the now constant-key Switch to its matched branch.
    let mut source = String::from("x = 3; multiIf(");
    for index in 1..=6 {
        source.push_str(&format!(r#"x == {index}, "v{index}", "#));
    }
    source.push_str(r#""default")"#);
    assert_eq!(
        optimized_result(&source),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("v3".into()))
    );
}

#[test]
fn collapsed_switch_evaluates_correctly() {
    let inner = int_switch(vec![(2, "b")], "d");
    let outer = OptExpr::Switch {
        id: NodeId::PLACEHOLDER,
        inputs: vec![OptExpr::Register(NodeId::PLACEHOLDER, 0)],
        table: vec![(
            Key::from_value(&Value::Int64(1)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("a".into())),
        )],
        default: Box::new(inner),
    };
    let merged = rewrite_result(outer, 1);
    assert_eq!(
        eval_with_register0(merged.clone(), Value::Int64(1)),
        Value::Text("a".into())
    );
    assert_eq!(
        eval_with_register0(merged.clone(), Value::Int64(2)),
        Value::Text("b".into())
    );
    assert_eq!(
        eval_with_register0(merged, Value::Int64(9)),
        Value::Text("d".into())
    );
}

/// Evaluate a `TypeAssert` over a constant operand directly (the rewrites need a
/// dynamic operand, so the rule output cannot be evaluated; this exercises the
/// node's runtime contract in isolation).
fn eval_type_assert(
    operand: Value,
    expect: TypeClass,
    on_present: AssertYield,
) -> Result<Value, EvalError> {
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::TypeAssert {
            id: NodeId::PLACEHOLDER,
            inner: Box::new(OptExpr::Const(NodeId::PLACEHOLDER, operand)),
            expect,
            on_present,
        },
        register_count: 0,
    };
    let compiled = compact(program).unwrap();
    eval_const_program(&compiled, &registry(), &context())
}

#[test]
fn type_assert_yields_const_for_in_class_operand() {
    let result = eval_type_assert(
        Value::Text("abc".to_string()),
        TypeClass::String,
        AssertYield::Const(Value::Bool(true)),
    );
    assert_eq!(result.unwrap(), Value::Bool(true));
}

#[test]
fn type_assert_identity_yields_the_operand() {
    let result = eval_type_assert(
        Value::Text("abc".to_string()),
        TypeClass::String,
        AssertYield::Identity,
    );
    assert_eq!(result.unwrap(), Value::Text("abc".to_string()));
}

#[test]
fn type_assert_propagates_null() {
    let result = eval_type_assert(
        Value::Null,
        TypeClass::String,
        AssertYield::Const(Value::Bool(true)),
    );
    assert_eq!(result.unwrap(), Value::Null);
}

#[test]
fn type_assert_errors_on_wrong_class() {
    // A present operand of the wrong class reproduces the eliminated op's error.
    let result = eval_type_assert(
        Value::Int64(1),
        TypeClass::String,
        AssertYield::Const(Value::Bool(true)),
    );
    assert!(matches!(
        result,
        Err(EvalError::TypeAssert {
            expected: "String",
            ..
        })
    ));
}

#[test]
fn collapses_encoding_round_trips_to_bytes_type_assert() {
    // bytesFromHex(hex(x)) and bytesFromBase64(base64(x)) → TypeAssert{Bytes,
    // Identity}: a total encode followed by its total decode is the identity.
    for source in [
        "bytesFromHex(hex(field(\"x\")))",
        "bytesFromBase64(base64(field(\"x\")))",
    ] {
        match optimized_result(source) {
            OptExpr::TypeAssert {
                inner,
                expect,
                on_present,
                ..
            } => {
                assert!(matches!(*inner, OptExpr::SourceField(_, name) if name == "x"));
                assert_eq!(expect, TypeClass::Bytes);
                assert_eq!(on_present, AssertYield::Identity);
            }
            other => panic!("{source}: expected a Bytes TypeAssert, got {other:?}"),
        }
    }
}

#[test]
fn keeps_bitnot_involution_untyped() {
    // bitNot coerces to i64 (widening narrow ints), so bitNot(bitNot(x)) is NOT a
    // type-preserving identity → left for the type-aware pass; BOTH calls survive.
    match optimized_result("~(~field(\"x\"))") {
        OptExpr::Call { args, .. } => match &args[0] {
            OptExpr::Call { args: inner, .. } => {
                assert!(matches!(&inner[0], OptExpr::SourceField(_, name) if name == "x"));
            }
            other => panic!("expected the inner bitNot to survive, got {other:?}"),
        },
        other => panic!("expected the outer bitNot call, got {other:?}"),
    }
}

#[test]
fn keeps_distinct_idempotent_ops() {
    // upper(lower(x)) — different ops, `lower` is NOT redundant under `upper`, so
    // the inner call must NOT be dropped.
    match optimized_result("upper(lower(field(\"x\")))") {
        OptExpr::Call { args, .. } => {
            assert!(
                matches!(&args[0], OptExpr::Call { .. }),
                "the inner lower must be kept"
            );
        }
        other => panic!("expected nested calls, got {other:?}"),
    }
}

#[test]
fn collapses_idempotent_string_ops() {
    // upper(upper(x)) / lower(lower(x)) / trim(trim(x)) → f(x): the outer call
    // survives (keeping its type check), the redundant inner one is dropped.
    for op in ["upper", "lower", "trim"] {
        let source = format!("{op}({op}(field(\"x\")))");
        match optimized_result(&source) {
            OptExpr::Call { args, .. } => {
                assert_eq!(args.len(), 1, "{op}: single call kept");
                assert!(
                    matches!(&args[0], OptExpr::SourceField(_, name) if name == "x"),
                    "{op}: inner duplicate dropped"
                );
            }
            other => panic!("{op}: expected a single call, got {other:?}"),
        }
    }
}

#[test]
fn type_assert_accepts_bool_and_bytes_classes() {
    assert_eq!(
        eval_type_assert(Value::Bool(true), TypeClass::Bool, AssertYield::Identity).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        eval_type_assert(
            Value::Bytes(vec![1, 2, 3]),
            TypeClass::Bytes,
            AssertYield::Identity
        )
        .unwrap(),
        Value::Bytes(vec![1, 2, 3])
    );
    // Wrong class for a Bool assert reproduces the eliminated op's TypeMismatch.
    assert!(matches!(
        eval_type_assert(
            Value::Int64(1),
            TypeClass::Bool,
            AssertYield::Const(Value::Bool(true))
        ),
        Err(EvalError::TypeAssert {
            expected: "Bool",
            ..
        })
    ));
}

#[test]
fn hoists_repeated_field_read_into_register() {
    // `field("a")` is read twice — materialize it once and reference it.
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse("field(\"a\") + field(\"a\")"),
        &registry,
        &context,
        true,
    )
    .unwrap();
    assert_eq!(program.statements.len(), 1);
    assert!(
        matches!(&program.statements[0].value, OptExpr::SourceField(_, name) if name == "a"),
        "expected a hoisted source-field binding"
    );
    let register = program.statements[0].register;
    match &program.result {
        OptExpr::Call { args, .. } => {
            assert_eq!(args.len(), 2);
            assert!(
                args.iter()
                    .all(|arg| matches!(arg, OptExpr::Register(_, reg) if *reg == register)),
                "both reads must reference the hoisted register"
            );
        }
        other => panic!("expected a Call result, got {other:?}"),
    }
}

#[test]
fn does_not_hoist_field_read_only_once() {
    // Distinct fields read once each: hoisting would only add indirection.
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse("field(\"a\") + field(\"b\")"),
        &registry,
        &context,
        true,
    )
    .unwrap();
    assert!(program.statements.is_empty());
}

#[test]
fn flattens_nested_variadic_concat() {
    // Nested concat with a non-constant field prevents full folding, so the
    // structural flattening is observable: one concat call, three arguments.
    let result = optimized_result("concat(concat(field(\"a\"), \"-\"), field(\"b\"))");
    match result {
        OptExpr::Call { args, .. } => assert_eq!(args.len(), 3),
        other => panic!("expected a flattened concat call, got {other:?}"),
    }
}

// ---- Ground-truth evaluation ----------------------------------------------

#[test]
fn evaluates_arithmetic() {
    assert_eq!(eval_unoptimized("1 + 2 * 3"), Value::Int64(7));
    assert_eq!(eval_unoptimized("10 - 4"), Value::Int64(6));
    assert_eq!(eval_unoptimized("min(3, 7)"), Value::Int64(3));
    assert_eq!(eval_unoptimized("max(3, 7)"), Value::Int64(7));
}

#[test]
fn evaluates_registers() {
    assert_eq!(eval_unoptimized("x = 5; x + 1"), Value::Int64(6));
    assert_eq!(eval_unoptimized("x = 3; y = x * 2; x + y"), Value::Int64(9));
    // Shadowing reads the earlier binding when computing the new one.
    assert_eq!(eval_unoptimized("x = 1; x = x + 1; x"), Value::Int64(2));
}

#[test]
fn evaluates_conditionals() {
    assert_eq!(eval_unoptimized("if(true, 10, 20)"), Value::Int64(10));
    assert_eq!(eval_unoptimized("if(false, 10, 20)"), Value::Int64(20));
    assert_eq!(
        eval_unoptimized("multiIf(1 == 2, 10, 2 == 2, 20, 30)"),
        Value::Int64(20)
    );
    assert_eq!(eval_unoptimized("ifNull(null, 42)"), Value::Int64(42));
    assert_eq!(eval_unoptimized("nullIf(5, 5)"), Value::Null);
}

#[test]
fn evaluates_boolean_short_circuit() {
    assert_eq!(eval_unoptimized("false && true"), Value::Bool(false));
    assert_eq!(eval_unoptimized("true || false"), Value::Bool(true));
    assert_eq!(eval_unoptimized("null || true"), Value::Bool(true));
    assert_eq!(eval_unoptimized("null && false"), Value::Bool(false));
}

#[test]
fn evaluates_interpolation() {
    let registry = registry();
    let context = context();
    let program = Parser::create().parse("value: {1 + 2}").unwrap();
    let compiled = compact(optimize(&program, &registry, &context, false).unwrap()).unwrap();
    assert_eq!(
        eval_const_program(&compiled, &registry, &context).unwrap(),
        Value::Text("value: 3".to_string())
    );
}

#[test]
fn optimized_and_unoptimized_agree_on_examples() {
    for source in [
        "1 + 2 * 3",
        "if(1 < 2, 10, 20)",
        "x = 5; x + 1",
        "min(max(1, 2), 3)",
        "concat(\"a\", concat(\"b\", \"c\"))",
    ] {
        let registry = registry();
        let context = context();
        let unoptimized =
            eval_const_program(&compile_unoptimized(source), &registry, &context).unwrap();
        let optimized =
            eval_const_program(&compile_optimized(source), &registry, &context).unwrap();
        assert_eq!(unoptimized, optimized, "mismatch for `{source}`");
    }
}

// ---- Three-valued boolean folding & evaluation ----------------------------

#[test]
fn folds_null_boolean_three_valued() {
    assert_eq!(
        optimized_result("null && false"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(false))
    );
    assert_eq!(
        optimized_result("null && true"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Null)
    );
    assert_eq!(
        optimized_result("null || true"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(true))
    );
    assert_eq!(
        optimized_result("null || false"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Null)
    );
}

#[test]
fn evaluates_null_boolean_three_valued() {
    assert_eq!(eval_unoptimized("null && false"), Value::Bool(false));
    assert_eq!(eval_unoptimized("null && true"), Value::Null);
    assert_eq!(eval_unoptimized("null || true"), Value::Bool(true));
    assert_eq!(eval_unoptimized("null || false"), Value::Null);
}

#[test]
fn prunes_if_null_and_null_if() {
    assert_eq!(
        optimized_result("ifNull(null, 42)"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(42))
    );
    assert_eq!(
        optimized_result("ifNull(7, field(\"x\"))"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(7))
    );
    assert_eq!(
        optimized_result("nullIf(5, 5)"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Null)
    );
    assert_eq!(
        optimized_result("nullIf(5, 6)"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(5))
    );
}

#[test]
fn prunes_multi_if_all_false_to_default() {
    assert_eq!(
        optimized_result("multiIf(false, 1, false, 2, 3)"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(3))
    );
}

// ---- Arena layout: interpolation & object ---------------------------------

#[test]
fn evaluates_object_via_arena() {
    // Unoptimized keeps the Object node, exercising the key-table + value-run
    // arena path in the evaluator.
    let value = eval_unoptimized("{\"a\" = 1, \"b\" = 1 + 1}");
    assert_eq!(object_field(&value, "a"), &Value::Int64(1));
    assert_eq!(object_field(&value, "b"), &Value::Int64(2));
}

#[test]
fn object_literal_is_consumable_by_object_builtins() {
    // An object literal evaluates to `Value::Object` (matching its resolved
    // `DataType::Object`), so the object builtins — which require `Value::Object`
    // — accept it. Before the literal lowered to `Value::Json`, this round-trip
    // raised a runtime `TypeMismatch` despite type-checking cleanly.
    assert_eq!(
        eval_unoptimized("objectGet({\"a\" = 1, \"b\" = 2}, \"b\")"),
        Value::Int64(2)
    );
    assert_eq!(
        eval_unoptimized("objectLength({\"a\" = 1, \"b\" = 2})"),
        Value::Int64(2)
    );
}

#[test]
fn object_get_hit_folds_to_the_matched_value() {
    // Dynamic-valued object → ObjectFold leaves it, ObjectAccessFold extracts
    // the matched value. (`field("x")` collapses to `SourceField`.)
    assert_eq!(
        optimized_result("objectGet({\"k\" = field(\"x\")}, \"k\")"),
        OptExpr::SourceField(NodeId::PLACEHOLDER, "x".to_string())
    );
}

#[test]
fn object_get_hit_drops_infallible_siblings() {
    // The matched value is returned; the other entry (`field("y")`, infallible)
    // is dropped.
    assert_eq!(
        optimized_result("objectGet({\"k\" = field(\"x\"), \"j\" = field(\"y\")}, \"k\")"),
        OptExpr::SourceField(NodeId::PLACEHOLDER, "x".to_string())
    );
}

#[test]
fn object_get_miss_folds_to_null() {
    assert_eq!(
        optimized_result("objectGet({\"k\" = field(\"x\")}, \"absent\")"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Null)
    );
}

#[test]
fn object_length_folds_to_count() {
    assert_eq!(
        optimized_result("objectLength({\"k\" = field(\"x\"), \"j\" = field(\"y\")})"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(2))
    );
}

#[test]
fn object_has_key_folds_to_bool() {
    assert_eq!(
        optimized_result("objectHasKey({\"k\" = field(\"x\")}, \"k\")"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(true))
    );
    assert_eq!(
        optimized_result("objectHasKey({\"k\" = field(\"x\")}, \"absent\")"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(false))
    );
}

#[test]
fn object_get_keeps_call_when_a_dropped_sibling_can_fail() {
    // The matched key is "k", but sibling "j" = `field("y") / field("z")` is
    // fallible (divide). Dropping it would discard its potential error, so the
    // rewrite must NOT fire — the call stays.
    let result = optimized_result(
        "objectGet({\"k\" = field(\"x\"), \"j\" = field(\"y\") / field(\"z\")}, \"k\")",
    );
    assert!(
        matches!(result, OptExpr::Call { .. }),
        "fallible sibling must block the fold, got {result:?}"
    );
}

#[test]
fn object_length_keeps_call_when_a_value_can_fail() {
    // A fallible value must still be evaluated; folding to the static count would
    // discard its potential error, so the call stays.
    let result = optimized_result("objectLength({\"k\" = field(\"y\") / field(\"z\")})");
    assert!(
        matches!(result, OptExpr::Call { .. }),
        "fallible value must block the length fold, got {result:?}"
    );
}

#[test]
fn object_has_key_keeps_call_when_a_value_can_fail() {
    let result = optimized_result("objectHasKey({\"k\" = field(\"y\") / field(\"z\")}, \"k\")");
    assert!(
        matches!(result, OptExpr::Call { .. }),
        "fallible value must block the hasKey fold, got {result:?}"
    );
}

#[test]
fn rejects_duplicate_object_keys_at_compile_time() {
    // The converter catches a repeated key before const-folding can collapse the
    // literal into an opaque constant — a static, compile-time failure.
    let err = Optimizer::create(&registry(), &context())
        .compile(&parse("{\"a\" = 1, \"a\" = 2}"), None, None)
        .unwrap_err();
    assert!(
        matches!(&err, OptimizeError::DuplicateObjectKey { key } if key == "a"),
        "expected DuplicateObjectKey, got {err:?}"
    );
}

// ---- Compaction: constant interning ---------------------------------------

#[test]
fn compaction_dedups_type_exact_constants() {
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(5)),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(5)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        1,
        "equal same-typed constants collapse"
    );
}

#[test]
fn compaction_keeps_cross_width_constants_distinct() {
    // Int8(5) and Int64(5) are numerically equal but differently typed; sharing
    // a slot would silently change the second node's resolved type.
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int8(5)),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(5)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        2,
        "cross-width constants stay distinct"
    );
}

#[test]
fn compaction_keeps_float_constants_distinct() {
    // Floats are never deduped: `Key` conflates `-0.0` with `0.0` and every NaN
    // payload, so collapsing them could drop the sign. Even bit-identical floats
    // take separate slots (they are excluded before the dedup key is computed).
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Float64(0.0)),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Float64(-0.0)),
                ),
                (
                    "c".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Float64(0.0)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        3,
        "float constants (incl. +0.0/-0.0) never share a slot"
    );
}

#[test]
fn compaction_keeps_decimal_scale_distinct() {
    use bigdecimal::BigDecimal;
    use std::str::FromStr;

    // `1.0` and `1.00` are numerically equal but carry different scale; `Key`
    // would conflate them, so decimals are excluded from dedup to preserve scale.
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(
                        NodeId::PLACEHOLDER,
                        Value::Decimal(BigDecimal::from_str("1.0").unwrap()),
                    ),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(
                        NodeId::PLACEHOLDER,
                        Value::Decimal(BigDecimal::from_str("1.00").unwrap()),
                    ),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        2,
        "decimals of differing scale stay distinct"
    );
}

#[test]
fn compaction_dedups_across_the_whole_pool() {
    // Dedup is whole-pool (hash-based), not a bounded trailing window: a
    // duplicate far past any old window still collapses. 130 distinct constants
    // then a repeat of the first → 130 slots, not 131.
    let mut entries: Vec<(String, OptExpr)> = (0..130)
        .map(|n| {
            (
                format!("k{n}"),
                OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(n)),
            )
        })
        .collect();
    entries.push((
        "dup".to_string(),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(0)),
    ));
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(NodeId::PLACEHOLDER, entries),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        130,
        "a duplicate far beyond the old 100-slot window still dedups"
    );
}

#[test]
fn compaction_keeps_distinct_same_typed_constants() {
    // Negative control for the dedup hit-branch: two genuinely different values
    // of the same type must NOT collapse (a too-coarse key would merge them).
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(5)),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(6)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        2,
        "distinct same-typed values stay apart"
    );
}

#[test]
fn compaction_keeps_ipv4_and_mapped_ipv6_distinct() {
    use std::net::{Ipv4Addr, Ipv6Addr};

    // `Key` treats `Ipv4(1.2.3.4)` and its v6-mapped `Ipv6` form as equal (same
    // hash + cross-equal). Only the `Discriminant` in the dedup key keeps them in
    // separate const slots — otherwise the second node's type would flip Ipv6→Ipv4.
    let v4 = Ipv4Addr::new(1, 2, 3, 4);
    let mapped: Ipv6Addr = v4.to_ipv6_mapped();
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Ipv4(v4)),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Ipv6(mapped)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(
        compiled.const_len(),
        2,
        "Ipv4 and its mapped Ipv6 are cross-equal under Key but type-distinct"
    );
}

#[test]
fn compaction_never_dedups_json_constants() {
    // `Json` has no `Key` projection (`Key::from_value` is `None`), so even two
    // identical JSON literals take separate slots rather than merging.
    let json = serde_json::json!({"k": 1});
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Json(json.clone())),
                ),
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Json(json)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = Compactor::create().compact(program).unwrap();
    assert_eq!(compiled.const_len(), 2, "JSON constants are never deduped");
}

// ---- Compaction: object-key interning -------------------------------------

#[test]
fn compaction_interns_object_keys_shared_across_objects() {
    // One object is built in a statement, another in the result; both use the
    // inner names "id"/"name", so interning collapses each to a single pool entry.
    // Pool = {first, second, id, name} = 4 distinct, not the 6 occurrences. The
    // eval then proves a shared `KeyId` still renders EACH object's own values —
    // i.e. the dedup does not bleed one object's value into the other.
    let inner = |id: i64, name: &str| {
        OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "id".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(id)),
                ),
                (
                    "name".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Text(name.to_string())),
                ),
            ],
        )
    };
    let program = OptProgram {
        statements: vec![OptStatement {
            register: 0,
            value: inner(1, "a"),
        }],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "first".to_string(),
                    OptExpr::Register(NodeId::PLACEHOLDER, 0),
                ),
                ("second".to_string(), inner(2, "b")),
            ],
        ),
        register_count: 1,
    };
    let compiled = compact(program).unwrap();
    assert_eq!(
        compiled.key_pool_len(),
        4,
        "inner names shared across the statement and the result intern once each"
    );

    let value = eval_const_program(&compiled, &registry(), &context()).unwrap();
    let first = object_field(&value, "first");
    let second = object_field(&value, "second");
    assert_eq!(object_field(first, "id"), &Value::Int64(1));
    assert_eq!(object_field(first, "name"), &Value::Text("a".to_string()));
    assert_eq!(object_field(second, "id"), &Value::Int64(2));
    assert_eq!(object_field(second, "name"), &Value::Text("b".to_string()));
}

#[test]
fn compaction_interns_duplicate_keys_within_one_object() {
    // A name repeated inside a single object interns to one pool entry; the run
    // still holds both occurrences (same id twice). This OptProgram is built
    // directly, bypassing the converter that rejects duplicate keys at compile
    // time — so eval still runs, and `Value::Object` (an ordered list) preserves
    // BOTH entries rather than de-duplicating.
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "dup".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(1)),
                ),
                (
                    "dup".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(2)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = compact(program).unwrap();
    assert_eq!(compiled.key_pool_len(), 1, "a repeated name interns once");

    let OptNode::Object { keys, .. } = compiled.node(compiled.result()) else {
        panic!("expected an Object result node");
    };
    assert_eq!(
        compiled.keys(*keys).len(),
        2,
        "the run keeps both occurrences"
    );

    let value = eval_const_program(&compiled, &registry(), &context()).unwrap();
    assert_eq!(
        object_entries(&value),
        &[
            ("dup".to_string(), Value::Int64(1)),
            ("dup".to_string(), Value::Int64(2)),
        ],
        "the ordered object keeps both occurrences in order"
    );
}

#[test]
fn compaction_preserves_object_key_run_order() {
    // Interning addresses keys by pool id, but each object's key run must keep
    // its original order and stay paired with the right value.
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(
            NodeId::PLACEHOLDER,
            vec![
                (
                    "b".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(1)),
                ),
                (
                    "a".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(2)),
                ),
                (
                    "c".to_string(),
                    OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(3)),
                ),
            ],
        ),
        register_count: 0,
    };
    let compiled = compact(program).unwrap();
    let OptNode::Object { keys, .. } = compiled.node(compiled.result()) else {
        panic!("expected an Object result node");
    };
    let names: Vec<&str> = compiled
        .keys(*keys)
        .iter()
        .map(|id| compiled.key_name(*id))
        .collect();
    assert_eq!(
        names,
        vec!["b", "a", "c"],
        "the key run keeps insertion order"
    );

    // And the key→value pairing survives interning.
    let value = eval_const_program(&compiled, &registry(), &context()).unwrap();
    assert_eq!(object_field(&value, "b"), &Value::Int64(1));
    assert_eq!(object_field(&value, "a"), &Value::Int64(2));
    assert_eq!(object_field(&value, "c"), &Value::Int64(3));
}

// ---- Depth guard ----------------------------------------------------------

#[test]
fn rejects_program_nested_past_the_depth_limit() {
    // A directly-constructed program can nest deeper than the parser allows;
    // the optimizer enforces the bound at its own boundary.
    let mut expr = Expr::Literal(LiteralValue::Int(1));
    for _ in 0..(MAX_EXPR_DEPTH + 5) {
        expr = Expr::FunctionCall {
            name: "toString".to_string(),
            args: vec![expr],
        };
    }
    let program = Program {
        statements: vec![],
        result: expr,
    };
    let result = Optimizer::create(&registry(), &context()).optimize(&program, false, None);
    assert!(matches!(result, Err(OptimizeError::NestingTooDeep { .. })));
}

// ---- Control flow: conditional flattening & switch lowering ---------------

#[test]
fn flattens_nested_if_chain_to_multi_if() {
    // if(a, x, if(b, y, z)) → multiIf(a, x, b, y; default z).
    let result =
        optimized_result("if(field(\"a\"), 1, if(field(\"b\"), 2, if(field(\"c\"), 3, 4)))");
    match result {
        OptExpr::MultiIf { branches, .. } => assert_eq!(branches.len(), 3),
        other => panic!("expected a flat multiIf, got {other:?}"),
    }
}

#[test]
fn lowers_large_equality_multi_if_to_switch() {
    let source = "multiIf(\
        field(\"x\") == 1, 10, field(\"x\") == 2, 20, field(\"x\") == 3, 30, \
        field(\"x\") == 4, 40, field(\"x\") == 5, 50, field(\"x\") == 6, 60, 0)";
    match optimized_result(source) {
        OptExpr::Switch { inputs, table, .. } => {
            assert_eq!(inputs.len(), 1);
            assert_eq!(table.len(), 6);
        }
        other => panic!("expected Switch, got {other:?}"),
    }
}

#[test]
fn lowers_composite_two_key_switch() {
    let source = "multiIf(\
        field(\"a\") == 1 && field(\"b\") == 1, 1, field(\"a\") == 1 && field(\"b\") == 2, 2, \
        field(\"a\") == 1 && field(\"b\") == 3, 3, field(\"a\") == 2 && field(\"b\") == 1, 4, \
        field(\"a\") == 2 && field(\"b\") == 2, 5, field(\"a\") == 2 && field(\"b\") == 3, 6, 0)";
    match optimized_result(source) {
        OptExpr::Switch { inputs, table, .. } => {
            assert_eq!(inputs.len(), 2);
            assert_eq!(table.len(), 6);
        }
        other => panic!("expected composite Switch, got {other:?}"),
    }
}

#[test]
fn expands_or_conditions_into_switch_entries() {
    let source = "multiIf(\
        field(\"x\") == 1 || field(\"x\") == 2, 100, field(\"x\") == 3, 30, \
        field(\"x\") == 4, 40, field(\"x\") == 5, 50, field(\"x\") == 6, 60, \
        field(\"x\") == 7, 70, 0)";
    match optimized_result(source) {
        OptExpr::Switch { inputs, table, .. } => {
            assert_eq!(inputs.len(), 1);
            // 6 branches, but the leading `or` contributes two entries → 7.
            assert_eq!(table.len(), 7);
        }
        other => panic!("expected Switch, got {other:?}"),
    }
}

#[test]
fn keeps_small_or_float_keyed_multi_if() {
    // Below the >5 threshold stays a multiIf.
    let small = "multiIf(field(\"x\") == 1, 10, field(\"x\") == 2, 20, \
        field(\"x\") == 3, 30, field(\"x\") == 4, 40, field(\"x\") == 5, 50, 0)";
    assert!(matches!(optimized_result(small), OptExpr::MultiIf { .. }));

    // Float keys are not allow-listed (NaN / float-equality), so no switch.
    let floats = "multiIf(field(\"x\") == 1.5, 10, field(\"x\") == 2.5, 20, \
        field(\"x\") == 3.5, 30, field(\"x\") == 4.5, 40, field(\"x\") == 5.5, 50, \
        field(\"x\") == 6.5, 60, 0)";
    assert!(matches!(optimized_result(floats), OptExpr::MultiIf { .. }));
}

#[test]
fn keeps_impure_keyed_multi_if_unlowered() {
    // Lowering a `multiIf` to a `Switch` evaluates the key ONCE; that is only sound
    // for a pure key. An impure key (`randomInt`) must keep the `multiIf`, which
    // re-evaluates the condition per branch.
    let source = "multiIf(\
        randomInt(0, 10) == 1, 10, randomInt(0, 10) == 2, 20, randomInt(0, 10) == 3, 30, \
        randomInt(0, 10) == 4, 40, randomInt(0, 10) == 5, 50, randomInt(0, 10) == 6, 60, 0)";
    assert!(
        matches!(optimized_result(source), OptExpr::MultiIf { .. }),
        "an impure-keyed multiIf must not be switch-lowered"
    );
}

#[test]
fn switch_lowering_keeps_first_branch_on_duplicate_key() {
    // Two branches share key 1; switch lowering is first-match (like the original
    // `multiIf`), so the table entry for 1 must keep the FIRST branch's value (10),
    // not the later 999.
    let source = "multiIf(\
        field(\"x\") == 1, 10, field(\"x\") == 1, 999, field(\"x\") == 2, 20, \
        field(\"x\") == 3, 30, field(\"x\") == 4, 40, field(\"x\") == 5, 50, \
        field(\"x\") == 6, 60, 0)";
    let OptExpr::Switch { table, .. } = optimized_result(source) else {
        panic!("expected a Switch");
    };
    let key_one = Key::single(Value::Int64(1)).unwrap();
    let matches: Vec<&OptExpr> = table
        .iter()
        .filter(|(key, _)| *key == key_one)
        .map(|(_, value)| value)
        .collect();
    assert_eq!(matches.len(), 1, "the duplicate key collapses to one entry");
    assert_eq!(
        *matches[0],
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(10)),
        "first branch wins"
    );
}

#[test]
fn evaluates_switch_dispatch() {
    // A const-keyed switch built directly (the optimizer would const-fold one
    // away), to exercise compaction of the table + `eval_switch`.
    let table = vec![
        (
            Key::single(Value::Int64(1)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(10)),
        ),
        (
            Key::single(Value::Int64(2)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(20)),
        ),
        (
            Key::single(Value::Int64(3)).unwrap(),
            OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(30)),
        ),
    ];
    let registry = registry();
    let context = context();
    let dispatch = |input: Value| {
        let program = OptProgram {
            statements: vec![],
            result: OptExpr::Switch {
                id: NodeId::PLACEHOLDER,
                inputs: vec![OptExpr::Const(NodeId::PLACEHOLDER, input)],
                table: table.clone(),
                default: Box::new(OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(0))),
            },
            register_count: 0,
        };
        let compiled = Compactor::create().compact(program).unwrap();
        eval_const_program(&compiled, &registry, &context).unwrap()
    };
    assert_eq!(dispatch(Value::Int64(2)), Value::Int64(20)); // hit
    assert_eq!(dispatch(Value::Int64(99)), Value::Int64(0)); // miss → default
}

#[test]
fn compiles_large_if_else_if_chain_to_a_switch() {
    // A long if/else-if equality ladder desugars to a flat multiIf at parse time,
    // then lowers to an O(1) Switch — exercising the raised node cap and the
    // hashed switch-table dedup. The key (`field("k")`, deterministic per row) is
    // collapsed to a single dispatch input.
    let count = 2000usize;
    // Built linearly (`if(...,` prefixes, default, then the `)` tail) rather than
    // re-formatting a growing accumulator, which would be quadratic.
    let mut chain = String::with_capacity(count * 28);
    for n in 0..count {
        chain.push_str(&format!("if(field(\"k\") == {n}, {n}, "));
    }
    chain.push('0');
    for _ in 0..count {
        chain.push(')');
    }
    let compiled = compile_optimized(&chain);
    let OptNode::Switch { table, .. } = compiled.node(compiled.result()) else {
        panic!(
            "expected the ladder to lower to a Switch, got {:?}",
            compiled.node(compiled.result())
        );
    };
    assert_eq!(
        compiled.switch_table(*table).branches().count(),
        count,
        "every distinct key becomes one dispatch entry"
    );
}

#[test]
fn if_else_if_chain_evaluates_like_the_nested_form() {
    // The parse-time desugar to multiIf preserves if/else-if semantics: the first
    // matching branch wins, otherwise the default.
    assert_eq!(
        eval_unoptimized("if(1 == 2, 10, if(3 == 3, 20, 30))"),
        Value::Int64(20)
    );
    assert_eq!(
        eval_unoptimized("if(1 == 1, 10, if(3 == 3, 20, 30))"),
        Value::Int64(10)
    );
    assert_eq!(
        eval_unoptimized("if(1 == 2, 10, if(3 == 4, 20, 30))"),
        Value::Int64(30)
    );
    // A null in a non-taken branch must not leak; a taken null branch yields null
    // (the desugar preserves the nested form's null behaviour, not just values).
    assert_eq!(
        eval_unoptimized("if(1 == 2, null, if(3 == 3, 20, 30))"),
        Value::Int64(20)
    );
    assert_eq!(
        eval_unoptimized("if(1 == 1, null, if(3 == 3, 20, 30))"),
        Value::Null
    );
}

// ---- Move annotation: register clone elision ------------------------------

/// The register-read argument (`Register` or `RegisterTake`) of a call node.
fn register_read_arg(program: &CompactProgram, call: NodeRef) -> &OptNode {
    let OptNode::Call { args, .. } = program.node(call) else {
        panic!("expected a call node, got {:?}", program.node(call));
    };
    program
        .args(*args)
        .iter()
        .map(|arg| program.node(*arg))
        .find(|node| matches!(node, OptNode::Register(_) | OptNode::RegisterTake(_)))
        .expect("a register-read argument")
}

fn register_of(node: &OptNode) -> RegisterId {
    match node {
        OptNode::Register(register) | OptNode::RegisterTake(register) => *register,
        other => panic!("expected a register read, got {other:?}"),
    }
}

#[test]
fn aliased_hoisted_register_in_one_call_is_not_moved() {
    // `field("c")` read twice in ONE call → field hoist binds it to a register;
    // both reads become `Register(r)`. Under the lazy `ArgWindow` a function may
    // take one argument before reading another, so a register read by two
    // arguments of the same call must NOT be moved — both stay clones (moving
    // either could strand the sibling read). See the move-annotator's call guard.
    let compiled = compile_optimized(r#"concat(field("c"), field("c"))"#);
    let register = compiled.statements()[0].register;
    let OptNode::Call { args, .. } = compiled.node(compiled.result()) else {
        panic!("expected a concat call result");
    };
    let reads: Vec<&OptNode> = compiled
        .args(*args)
        .iter()
        .map(|arg| compiled.node(*arg))
        .collect();
    assert!(
        matches!(reads[0], OptNode::Register(r) if *r == register),
        "the first read clones: {:?}",
        reads[0]
    );
    assert!(
        matches!(reads[1], OptNode::Register(r) if *r == register),
        "the aliased second read also clones (not moved): {:?}",
        reads[1]
    );
}

#[test]
fn move_marks_register_last_use_in_both_if_branches() {
    // `x` is an impure binding (so it survives as a register) read once in each
    // mutually-exclusive arm. Each read is the register's last use on its path,
    // so both arms move — branch-aware liveness gives each arm the parent's
    // live-after, not the sibling's read.
    let compiled = compile_optimized(
        r#"x = randomHex(4); y = randomInt(0, 10); if(y == 5, concat(x, "a"), concat(x, "b"))"#,
    );
    let OptNode::If {
        then_branch,
        else_branch,
        ..
    } = compiled.node(compiled.result())
    else {
        panic!("expected an if result");
    };
    let then_read = register_read_arg(&compiled, *then_branch);
    let else_read = register_read_arg(&compiled, *else_branch);
    assert!(
        matches!(then_read, OptNode::RegisterTake(_)),
        "the then arm moves: {then_read:?}"
    );
    assert!(
        matches!(else_read, OptNode::RegisterTake(_)),
        "the else arm moves: {else_read:?}"
    );
    assert_eq!(
        register_of(then_read),
        register_of(else_read),
        "both arms read the same register"
    );
}

#[test]
fn keeps_register_read_in_controlling_condition_as_clone() {
    // `x` is read in the condition AND the then-arm. The condition runs first and
    // the then-arm may follow it, so the condition's read must clone; only the
    // then-arm's read is the register's last use and moves.
    let compiled = compile_optimized(r#"x = randomInt(0, 10); if(x == 5, toString(x), "default")"#);
    let OptNode::If {
        condition,
        then_branch,
        ..
    } = compiled.node(compiled.result())
    else {
        panic!("expected an if result");
    };
    let condition_read = register_read_arg(&compiled, *condition);
    let then_read = register_read_arg(&compiled, *then_branch);
    assert!(
        matches!(condition_read, OptNode::Register(_)),
        "the condition read clones: {condition_read:?}"
    );
    assert!(
        matches!(then_read, OptNode::RegisterTake(_)),
        "the then-arm read moves: {then_read:?}"
    );
    assert_eq!(
        register_of(condition_read),
        register_of(then_read),
        "both reads are of the same register"
    );
}

#[test]
fn aliased_register_reads_in_one_call_both_clone_and_agree() {
    // `concat(x, x)` reuses one impure register in two arguments of one call.
    // The aliasing guard forbids moving either read, so both clone; the two
    // halves of the result must then be equal (neither read corrupts the other).
    let compiled = compile_optimized(r#"x = randomHex(4); concat(x, x)"#);
    let OptNode::Call { args, .. } = compiled.node(compiled.result()) else {
        panic!("expected a concat call result");
    };
    let reads: Vec<&OptNode> = compiled
        .args(*args)
        .iter()
        .map(|arg| compiled.node(*arg))
        .collect();
    assert!(matches!(reads[0], OptNode::Register(_)), "{:?}", reads[0]);
    assert!(
        matches!(reads[1], OptNode::Register(_)),
        "the aliased second read also clones: {:?}",
        reads[1]
    );

    let value = eval_const_program(&compiled, &registry(), &context()).unwrap();
    let Value::Text(text) = value else {
        panic!("expected text, got {value:?}");
    };
    let half = text.len() / 2;
    assert!(half > 0, "randomHex(4) renders a non-empty string");
    assert_eq!(
        &text[..half],
        &text[half..],
        "the move on the second read must not corrupt the first read's clone"
    );
}

#[test]
fn keeps_always_run_operand_clone_under_short_circuit() {
    // `x` is read in BOTH operands of `||`. The left always runs; the right runs
    // only when the left is false. So the left's read must clone (the right may
    // re-read it), and only the right's read — the conservative last use — moves.
    let compiled = compile_optimized(r#"x = randomInt(0, 10); (x == 5) || (x == 6)"#);
    let OptNode::Or { left, right } = compiled.node(compiled.result()) else {
        panic!("expected an or result");
    };
    let left_read = register_read_arg(&compiled, *left);
    let right_read = register_read_arg(&compiled, *right);
    assert!(
        matches!(left_read, OptNode::Register(_)),
        "the always-run left clones: {left_read:?}"
    );
    assert!(
        matches!(right_read, OptNode::RegisterTake(_)),
        "the maybe-run right moves: {right_read:?}"
    );
    assert_eq!(register_of(left_read), register_of(right_read));
}

#[test]
fn clones_value_read_when_ifnull_alternative_reads_register() {
    // `ifNull(upper(x), lower(x))`: `x` in the always-run value must clone because
    // the lazy alternative re-reads it; the alternative's read is the last use and
    // moves. The value is `upper(x)` (not a bare operand) so guard propagation does
    // not fold the alternative — keeping both register reads live for this test.
    let compiled = compile_optimized(r#"x = randomHex(4); ifNull(upper(x), lower(x))"#);
    let OptNode::IfNull { value, alternative } = compiled.node(compiled.result()) else {
        panic!("expected an ifNull result");
    };
    let value_read = register_read_arg(&compiled, *value);
    let alternative_read = register_read_arg(&compiled, *alternative);
    assert!(
        matches!(value_read, OptNode::Register(_)),
        "the always-run value clones: {value_read:?}"
    );
    assert!(
        matches!(alternative_read, OptNode::RegisterTake(_)),
        "the lazy alternative moves: {alternative_read:?}"
    );
    assert_eq!(register_of(value_read), register_of(alternative_read));
}

#[test]
fn marks_only_the_last_condition_read_in_a_multi_if_chain() {
    // The conditions of a `multiIf` run in sequence (each only if the earlier
    // ones missed), so the same register read in several conditions clones in the
    // earlier ones and moves only in the last — the carry-fold of the chain.
    let compiled = compile_optimized(
        r#"x = randomInt(0, 100); multiIf(x > 1, "a", x > 2, "b", x > 3, "c", "d")"#,
    );
    let OptNode::MultiIf { branches, .. } = compiled.node(compiled.result()) else {
        panic!("expected a multiIf result");
    };
    // Flattened branches: [c0, v0, c1, v1, c2, v2].
    let branch_refs = compiled.args(*branches);
    let first_condition = register_read_arg(&compiled, branch_refs[0]);
    let last_condition = register_read_arg(&compiled, branch_refs[4]);
    assert!(
        matches!(first_condition, OptNode::Register(_)),
        "an earlier condition clones: {first_condition:?}"
    );
    assert!(
        matches!(last_condition, OptNode::RegisterTake(_)),
        "the last condition moves: {last_condition:?}"
    );
}

#[test]
fn marks_moves_across_live_set_word_boundary() {
    // 65 distinct fields, each read twice, hoist to registers r0..r64 — spanning
    // two 64-bit `LiveSet` words. Each register's second read is its last use, so
    // every one moves, including those in the high word (>= 64). An interpolation
    // is used (not one big call) because function arguments cap at 128; field
    // names are single-quoted to avoid nesting double quotes in the template. Each
    // segment renders the register through a `toString` call, so the read is the
    // call's argument.
    let mut template = String::from("\"");
    for index in 0..65 {
        template.push_str(&format!("{{field('f{index}')}}{{field('f{index}')}}"));
    }
    template.push('"');
    let compiled = compile_optimized(&template);
    assert!(
        compiled.register_count() >= 65,
        "expected at least 65 hoisted registers, got {}",
        compiled.register_count()
    );

    let OptNode::Interpolation(segments) = compiled.node(compiled.result()) else {
        panic!("expected an interpolation result");
    };
    let takes: Vec<RegisterId> = compiled
        .args(*segments)
        .iter()
        .filter_map(|segment| match register_read_arg(&compiled, *segment) {
            OptNode::RegisterTake(register) => Some(*register),
            _ => None,
        })
        .collect();
    assert_eq!(
        takes.len(),
        compiled.register_count() as usize,
        "every register's last read moves"
    );
    assert!(
        takes.iter().any(|register| *register >= 64),
        "a move must land in the second LiveSet word"
    );
}

// ---- Property: optimization preserves meaning -----------------------------

fn values_equivalent(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Float64(a), Value::Float64(b)) if a.is_nan() && b.is_nan() => true,
        (Value::Float32(a), Value::Float32(b)) if a.is_nan() && b.is_nan() => true,
        _ => left == right,
    }
}

/// A recursive strategy generating well-typed value expressions. Leaves mix
/// small integers, `null` (to exercise null propagation and the three-valued
/// boolean/conditional arms), and `i64::MAX` (to drive overflow → BigInt
/// promotion on both the optimized and unoptimized paths). Nodes cover
/// arithmetic, unary negate, `min`/`max`, and `if` with comparison/boolean
/// (including null-bearing) conditions.
fn value_expression() -> impl Strategy<Value = String> {
    let leaf = prop_oneof![
        6 => (0i64..10).prop_map(|n| n.to_string()),
        1 => Just("null".to_string()),
        1 => Just(i64::MAX.to_string()),
    ];
    leaf.prop_recursive(4, 48, 3, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} + {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} - {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("({a} * {b})")),
            inner.clone().prop_map(|a| format!("(-{a})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("min({a}, {b})")),
            (inner.clone(), inner.clone()).prop_map(|(a, b)| format!("max({a}, {b})")),
            (inner.clone(), inner.clone(), inner.clone(), inner.clone())
                .prop_map(|(a, b, t, e)| format!("if(({a} < {b}), {t}, {e})")),
            (
                inner.clone(),
                inner.clone(),
                inner.clone(),
                inner.clone(),
                inner.clone(),
            )
                .prop_map(|(a, b, c, t, e)| format!("if((({a} == {b}) && ({c} < {a})), {t}, {e})")),
            // The brace-block branch form: a scoped binding read twice (nested
            // blocks rebind the same name, exercising scope save/restore).
            (inner.clone(), inner.clone(), inner.clone(), inner.clone()).prop_map(
                |(a, b, t, e)| {
                    format!("if (({a} < {b})) {{ v = {t}; (v + (v * {e})) }} else (-{e})")
                }
            ),
        ]
    })
}

proptest! {
    #[test]
    fn optimization_preserves_meaning(source in value_expression()) {
        let registry = registry();
        let context = context();
        let program = Parser::create().parse_expression(&source).unwrap();

        let unoptimized = compact(optimize(&program, &registry, &context, false).unwrap()).unwrap();
        let optimized = match Optimizer::create(&registry, &context).compile(&program, None, None) {
            Ok(program) => program,
            // The optimizer evaluates constants while folding; a constant in an
            // always-reached (eager) position that fails to evaluate stops the
            // build. The unoptimized program must then also fail when evaluated —
            // same erroring expression, just deferred to runtime.
            Err(_) => {
                prop_assert!(
                    eval_const_program(&unoptimized, &registry, &context).is_err(),
                    "optimizer rejected `{source}` but it evaluates cleanly unoptimized"
                );
                return Ok(());
            }
        };

        let from_unoptimized = eval_const_program(&unoptimized, &registry, &context);
        let from_optimized = eval_const_program(&optimized, &registry, &context);

        match (from_unoptimized, from_optimized) {
            (Ok(left), Ok(right)) => prop_assert!(
                values_equivalent(&left, &right),
                "value mismatch for `{source}`: {left:?} vs {right:?}"
            ),
            // Both error (e.g. integer overflow): consistent.
            (Err(_), Err(_)) => {}
            (left, right) => prop_assert!(
                false,
                "ok/err mismatch for `{source}`: {left:?} vs {right:?}"
            ),
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 3b — static type pass: schema-driven field typing + output validation.
// Exercised end-to-end through `compile(program, schema, expected)`.
// ---------------------------------------------------------------------------

fn make_schema(fields: &[(&str, DataType, bool)]) -> Schema {
    let fields = fields
        .iter()
        .map(|(name, data_type, nullable)| Field {
            name: (*name).to_owned(),
            data_type: data_type.clone(),
            nullable: *nullable,
        })
        .collect();
    Schema::new(fields)
}

fn compile_typed(
    source: &str,
    schema: Option<&Schema>,
    expected: Option<&ExpectedOutput>,
) -> Result<CompactProgram, OptimizeError> {
    Optimizer::create(&registry(), &context()).compile(&parse(source), schema, expected)
}

/// Optimize a program and derive its static type map against a schema. Returns
/// the optimized heap program alongside the map so a test can assert per-node
/// coverage.
fn type_map_of(source: &str, schema: &Schema) -> (OptProgram, TypeMap) {
    let registry = registry();
    let context = context();
    let optimized = optimize(&parse(source), &registry, &context, true).unwrap();
    let map = TypeChecker::create(&registry, Some(schema), optimized.register_count, false)
        .check(&optimized, None)
        .unwrap();
    (optimized, map)
}

/// Total node count of a program (every statement value plus the result).
fn program_node_count(program: &OptProgram) -> usize {
    let statement_nodes: usize = program
        .statements
        .iter()
        .map(|statement| statement.value.node_count())
        .sum();
    statement_nodes + program.result.node_count()
}

#[test]
fn typed_field_arithmetic_compiles_with_schema() {
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let result = compile_typed("field(\"x\") + 1", Some(&schema), None);
    assert!(result.is_ok(), "expected ok, got {result:?}");
}

#[test]
fn field_absent_from_fixed_schema_is_an_error() {
    let schema = make_schema(&[("a", DataType::Int64, false)]);
    let err = compile_typed("field(\"b\")", Some(&schema), None).unwrap_err();
    assert!(
        matches!(err, OptimizeError::FieldNotInSchema { ref name } if name == "b"),
        "got {err:?}"
    );
}

#[test]
fn output_type_text_into_int_column_fails() {
    let expected = ExpectedOutput {
        data_type: DataType::Int64,
        truncate: false,
    };
    let err = compile_typed("'hello'", None, Some(&expected)).unwrap_err();
    assert!(
        matches!(err, OptimizeError::OutputTypeMismatch { .. }),
        "got {err:?}"
    );
}

#[test]
fn output_type_widening_is_compatible() {
    // `1 + 1` folds to a small constant (bound 2 → materializes to Int8), so
    // widening to an Int64 column is compatible.
    let expected = ExpectedOutput {
        data_type: DataType::Int64,
        truncate: false,
    };
    assert!(compile_typed("1 + 1", None, Some(&expected)).is_ok());
}

#[test]
fn output_type_narrowing_requires_truncate() {
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let into_int8 = |truncate| ExpectedOutput {
        data_type: DataType::Int8,
        truncate,
    };
    // Int64 field → Int8 sink: rejected without truncate, accepted with it.
    let strict = compile_typed("field(\"x\")", Some(&schema), Some(&into_int8(false)));
    assert!(
        matches!(strict, Err(OptimizeError::OutputTypeMismatch { .. })),
        "got {strict:?}"
    );
    let truncating = compile_typed("field(\"x\")", Some(&schema), Some(&into_int8(true)));
    assert!(truncating.is_ok(), "got {truncating:?}");
}

#[test]
fn small_int_literal_materializes_into_narrow_column() {
    // `5` carries bound 3 → materializes to Int8, so it fits an Int8 column with
    // no truncate. Without the int-bound path this would be Int64 → Int8 and fail.
    let expected = ExpectedOutput {
        data_type: DataType::Int8,
        truncate: false,
    };
    assert!(compile_typed("5", None, Some(&expected)).is_ok());
}

#[test]
fn schemaless_field_does_not_error_and_skips_output_check() {
    // No schema: `field("x")` is unknown, so the result type is unknown and the
    // output-compat check is deferred to the runtime asserts — compiles clean
    // even against a concrete expected type.
    let expected = ExpectedOutput {
        data_type: DataType::Int64,
        truncate: false,
    };
    let result = compile_typed("field(\"x\") + 1", None, Some(&expected));
    assert!(result.is_ok(), "got {result:?}");
}

#[test]
fn register_binding_is_typed_from_its_definition() {
    // `x` read twice keeps a register; typing flows from the field through the
    // register into the arithmetic and the output check.
    let schema = make_schema(&[("a", DataType::Int32, false)]);
    let expected = ExpectedOutput {
        data_type: DataType::Int64,
        truncate: false,
    };
    let result = compile_typed("x = field(\"a\"); x + x", Some(&schema), Some(&expected));
    assert!(result.is_ok(), "got {result:?}");
}

#[test]
fn untyped_path_skips_the_type_pass() {
    // No schema and no expected output → the type pass does not run, so a
    // would-be output mismatch (`'hello'` into nothing) compiles, preserving the
    // untyped/comptime behaviour.
    assert!(compile_typed("'hello'", None, None).is_ok());
}

// ---- The static type map (sole-author derivation) -------------------------

#[test]
fn type_map_covers_every_node_under_a_fixed_schema() {
    // With a fixed schema every node is statically known, so the type-check pass
    // records a type for each one — the map is total. The keys are unique
    // per-compile ids, so its size equals the program's node count.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let (program, map) = type_map_of("field(\"x\") + 1", &schema);
    assert_eq!(
        map.len(),
        program_node_count(&program),
        "every node should have exactly one type entry under a fixed schema"
    );
}

#[test]
fn type_map_roots_at_the_result_type() {
    // The map entry for the result node's id is the program's output type: a bare
    // `field("x")` pass-through carries the schema field's type straight through.
    let schema = make_schema(&[("x", DataType::Text { size: None }, false)]);
    let (program, map) = type_map_of("field(\"x\")", &schema);
    let root_type = map
        .get(&program.result.id())
        .expect("the result node is typed under a fixed schema");
    assert_eq!(root_type.data_type, DataType::Text { size: None });
}

#[test]
fn type_map_covers_register_bindings() {
    // A register binding is typed once and every node — the binding value and the
    // result — is mapped.
    let schema = make_schema(&[("a", DataType::Int32, false)]);
    let (program, map) = type_map_of("x = field(\"a\"); x + x", &schema);
    assert_eq!(map.len(), program_node_count(&program));
    assert_eq!(map[&program.result.id()].data_type, DataType::Int64);
}

// ---- Typed discharge (the map's consumer) ---------------------------------

#[test]
fn typed_path_strips_a_redundant_string_assert() {
    // `concat(x, "")` parks a `TypeAssert{String}` over `x`. With `x` typed as
    // `Text`, the assert is statically satisfied, so discharge collapses it to the
    // bare field read.
    let schema = make_schema(&[("x", DataType::Text { size: None }, false)]);
    let program = compile_typed("concat(field(\"x\"), \"\")", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::SourceField(_)),
        "expected the assert discharged to a field read, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn untyped_path_keeps_the_string_assert() {
    // Without a schema the operand type is unknown, so the runtime `TypeAssert`
    // survives to do the per-row check — discharge never runs.
    let program = compile_typed("concat(field(\"x\"), \"\")", None, None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::TypeAssert { .. }),
        "the untyped path keeps the runtime assert, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_path_strips_redundant_to_string() {
    // `toString(x)` where `x` is already `Text` is the identity, so discharge
    // collapses it to the field read.
    let schema = make_schema(&[("x", DataType::Text { size: None }, false)]);
    let program = compile_typed("toString(field(\"x\"))", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::SourceField(_)),
        "toString of a Text field is the identity, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_path_keeps_to_string_of_a_non_text_field() {
    // `toString(x)` where `x` is Int64 is a real conversion — it must NOT be
    // stripped.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let program = compile_typed("toString(field(\"x\"))", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "a real toString conversion must survive, got {:?}",
        program.node(program.result())
    );
}

// ---- Const-yield discharge + value equivalence ----------------------------

/// A field source binding exactly one column, for the discharge differential.
struct OneField {
    name: String,
    value: Value,
}

impl FieldSource for OneField {
    fn field(&self, name: &str) -> Result<Value, EvalError> {
        if name == self.name {
            Ok(self.value.clone())
        } else {
            Err(EvalError::FieldNotBound)
        }
    }

    fn fields(&self, _selector: &air_elt_expr_parse::FieldsSelector) -> Result<Value, EvalError> {
        Err(EvalError::FieldNotBound)
    }
}

fn eval_with_field(program: &CompactProgram, name: &str, value: Value) -> Result<Value, EvalError> {
    let registry = registry();
    let context = context();
    let fields = OneField {
        name: name.to_owned(),
        value,
    };
    ProgramEvaluator::create_with_fields(program, &registry, &context, &fields).evaluate()
}

#[test]
fn typed_path_strips_const_yield_assert_for_a_non_null_operand() {
    // `contains(x, "")` parks a `TypeAssert{String, Const(true)}`. With `x` a
    // NON-null Text field the operand is always present and of class, so the
    // assert yields the constant unconditionally — discharge folds it to `true`.
    let schema = make_schema(&[("x", DataType::Text { size: None }, false)]);
    let program = compile_typed("contains(field(\"x\"), \"\")", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Const(_)),
        "non-null operand: the Const-yield assert folds to the constant, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_path_keeps_const_yield_assert_for_a_nullable_operand() {
    // With `x` a NULLABLE Text field the assert still distinguishes present
    // (`true`) from null (`null`), so it must NOT collapse to a bare constant.
    let schema = make_schema(&[("x", DataType::Text { size: None }, true)]);
    let program = compile_typed("contains(field(\"x\"), \"\")", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::TypeAssert { .. }),
        "nullable operand keeps the Const-yield assert, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn discharge_preserves_value_on_schema_consistent_rows() {
    // The discharge differential: on schema-consistent rows the typed (discharged)
    // and untyped (assert kept) programs evaluate to the same value. A non-null
    // Text schema admits only non-null Text rows; a nullable schema admits null
    // too, where the kept assert and the (non-stripped) typed program both yield
    // null.
    let cases: &[(bool, &[Value])] = &[
        (
            false,
            &[Value::Text("abc".to_owned()), Value::Text(String::new())],
        ),
        (true, &[Value::Text("hi".to_owned()), Value::Null]),
    ];
    for (nullable, rows) in cases {
        let schema = make_schema(&[("x", DataType::Text { size: None }, *nullable)]);
        let typed = compile_typed("contains(field(\"x\"), \"\")", Some(&schema), None).unwrap();
        let untyped = compile_typed("contains(field(\"x\"), \"\")", None, None).unwrap();
        for row in *rows {
            let from_typed = eval_with_field(&typed, "x", row.clone()).unwrap();
            let from_untyped = eval_with_field(&untyped, "x", row.clone()).unwrap();
            assert_eq!(
                from_typed, from_untyped,
                "discharge changed the value for nullable={nullable} row {row:?}"
            );
        }
    }
}

#[test]
fn type_map_covers_a_lowered_switch_with_reided_clones() {
    // An `||`-clause branch lowers to several `Switch` keys pointing at clones of
    // one branch value; `switch_lower` re-ids each clone, so every surviving node
    // keeps a unique id and the map stays total (`len == node_count`).
    let schema = make_schema(&[("k", DataType::Int64, false)]);
    let source = "multiIf(\
        field(\"k\") == 1 || field(\"k\") == 2, 100, \
        field(\"k\") == 3, 30, \
        field(\"k\") == 4, 40, \
        field(\"k\") == 5, 50, \
        field(\"k\") == 6, 60, \
        field(\"k\") == 7, 70, 0)";
    let (program, map) = type_map_of(source, &schema);
    assert!(
        matches!(program.result, OptExpr::Switch { .. }),
        "expected the multiIf to lower to a Switch, got {:?}",
        program.result
    );
    assert_eq!(
        map.len(),
        program_node_count(&program),
        "re-ided switch-branch clones keep every node id unique"
    );
}

// ---- Typed casts -----------------------------------------------------------

#[test]
fn typed_strips_a_redundant_int_cast() {
    // `toInt64(x)` where `x` is already `Int64` is the identity.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let program = compile_typed("toInt64(field(\"x\"))", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::SourceField(_)),
        "got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_keeps_a_widening_int_cast() {
    // `toInt64(x)` where `x` is `Int32` is a real widening (resolved type would
    // change from Int64 to Int32) — it must NOT be stripped.
    let schema = make_schema(&[("x", DataType::Int32, false)]);
    let program = compile_typed("toInt64(field(\"x\"))", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "a widening cast must survive, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_strips_redundant_float_and_bool_casts() {
    for (source, data_type) in [
        ("toFloat64(field(\"x\"))", DataType::Float64),
        ("toBool(field(\"x\"))", DataType::Bool),
        ("toUuid(field(\"x\"))", DataType::Uuid),
    ] {
        let schema = make_schema(&[("x", data_type.clone(), false)]);
        let program = compile_typed(source, Some(&schema), None).unwrap();
        assert!(
            matches!(program.node(program.result()), OptNode::SourceField(_)),
            "{source} over {data_type:?}: got {:?}",
            program.node(program.result())
        );
    }
}

// ---- Typed min/max flatten -------------------------------------------------

#[test]
fn typed_flattens_same_type_min_max() {
    // `max(max(a,b),c)` over uniform Int64 flattens to one 3-ary `max`.
    let schema = make_schema(&[
        ("a", DataType::Int64, false),
        ("b", DataType::Int64, false),
        ("c", DataType::Int64, false),
    ]);
    let program = compile_typed(
        "max(max(field(\"a\"), field(\"b\")), field(\"c\"))",
        Some(&schema),
        None,
    )
    .unwrap();
    let OptNode::Call { args, .. } = program.node(program.result()) else {
        panic!(
            "expected a max call, got {:?}",
            program.node(program.result())
        );
    };
    assert_eq!(
        program.args(*args).len(),
        3,
        "the nested max should flatten to 3 args"
    );
}

#[test]
fn typed_keeps_mixed_int_float_min_max_nested() {
    // A `Float64` operand mixed with integers triggers the lossy compare arm, so
    // the flatten must NOT fire — the result stays a 2-ary `max`.
    let schema = make_schema(&[
        ("a", DataType::Int64, false),
        ("b", DataType::Int64, false),
        ("c", DataType::Float64, false),
    ]);
    let program = compile_typed(
        "max(max(field(\"a\"), field(\"b\")), field(\"c\"))",
        Some(&schema),
        None,
    )
    .unwrap();
    let OptNode::Call { args, .. } = program.node(program.result()) else {
        panic!(
            "expected a max call, got {:?}",
            program.node(program.result())
        );
    };
    assert_eq!(
        program.args(*args).len(),
        2,
        "mixed int/float max must not flatten"
    );
}

// ---- Untyped min/max NULL-literal drop -------------------------------------

#[test]
fn drops_null_literal_from_max_to_single_operand() {
    // `max(x, null)` drops the null literal; the one-argument extremum that
    // remains is just `x`. Untyped (no schema) — the rule needs no type map.
    let program = compile_typed("max(field(\"x\"), null)", None, None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::SourceField(_)),
        "max(x, null) collapses to x, got {:?}",
        program.node(program.result())
    );
    assert_eq!(
        eval_with_field(&program, "x", Value::Int64(7)).unwrap(),
        Value::Int64(7)
    );
}

#[test]
fn drops_only_null_literal_from_min() {
    // `min(a, null, b)` drops only the null literal, leaving a 2-ary `min`.
    let program = compile_typed("min(field(\"a\"), null, field(\"b\"))", None, None).unwrap();
    let OptNode::Call { args, .. } = program.node(program.result()) else {
        panic!(
            "expected a min call, got {:?}",
            program.node(program.result())
        );
    };
    assert_eq!(
        program.args(*args).len(),
        2,
        "only the null literal is dropped"
    );
}

#[test]
fn all_null_max_folds_to_null_constant() {
    let program = compile_typed("max(null, null)", None, None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Const(_)),
        "max(null, null) folds to a null constant, got {:?}",
        program.node(program.result())
    );
    assert_eq!(
        eval_const_program(&program, &registry(), &context()).unwrap(),
        Value::Null
    );
}

#[test]
fn null_drop_lets_mixed_extremum_type_check() {
    // The null literal resolves to `Bool`, so without the drop `max(int_field,
    // null)` would fail `comparable_join` (Int64 vs Bool). Dropping the null first
    // leaves a well-typed extremum (here, just the field), so the typed path
    // compiles cleanly against a schema.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let program = compile_typed("max(field(\"x\"), null)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::SourceField(_)),
        "got {:?}",
        program.node(program.result())
    );
}

// ---- Typed min/max saturation ----------------------------------------------

#[test]
fn typed_saturates_max_at_type_maximum() {
    // `max(int8, 127)`: 127 is `Int8::MAX`, so the field can never exceed it — the
    // max collapses to the constant, value-exact against the unreduced form.
    let schema = make_schema(&[("x", DataType::Int8, false)]);
    let program = compile_typed("max(field(\"x\"), 127)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Const(_)),
        "max(int8, 127) saturates to 127, got {:?}",
        program.node(program.result())
    );
    let untyped = compile_typed("max(field(\"x\"), 127)", None, None).unwrap();
    assert_eq!(
        eval_with_field(&program, "x", Value::Int8(5)).unwrap(),
        eval_with_field(&untyped, "x", Value::Int8(5)).unwrap(),
    );
}

#[test]
fn typed_saturates_min_at_unsigned_zero() {
    // `min(uint8, 0)`: 0 is `UInt8::MIN`, so the field is always ≥ 0 — the min
    // collapses to 0.
    let schema = make_schema(&[("x", DataType::UInt8, false)]);
    let program = compile_typed("min(field(\"x\"), 0)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Const(_)),
        "min(uint8, 0) saturates to 0, got {:?}",
        program.node(program.result())
    );
    let untyped = compile_typed("min(field(\"x\"), 0)", None, None).unwrap();
    assert_eq!(
        eval_with_field(&program, "x", Value::UInt8(200)).unwrap(),
        eval_with_field(&untyped, "x", Value::UInt8(200)).unwrap(),
    );
}

#[test]
fn typed_saturates_over_nullable_operand() {
    // min/max skip NULLs, so saturation fires even for a NULLABLE operand: the
    // result is the bound whether `x` is present (≤ bound) or NULL (skipped).
    // This is the case the weaker `!can_fail` gate admits where `is_droppable`
    // (which requires non-null) would not.
    let schema = make_schema(&[("x", DataType::Int8, true)]);
    let program = compile_typed("max(field(\"x\"), 127)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Const(_)),
        "saturation fires over a nullable operand, got {:?}",
        program.node(program.result())
    );
    let untyped = compile_typed("max(field(\"x\"), 127)", None, None).unwrap();
    assert_eq!(
        eval_with_field(&program, "x", Value::Null).unwrap(),
        eval_with_field(&untyped, "x", Value::Null).unwrap(),
    );
}

#[test]
fn typed_does_not_saturate_below_bound() {
    // `max(int8, 100)`: 100 is NOT `Int8::MAX` (127), so the field can exceed it —
    // the max must survive.
    let schema = make_schema(&[("x", DataType::Int8, false)]);
    let program = compile_typed("max(field(\"x\"), 100)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "100 < Int8::MAX must not saturate, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_does_not_saturate_wider_operand() {
    // `max(int32, 127)`: 127 is `Int8::MAX` but not `Int32::MAX`, and the data
    // types differ — both the bound and the type-match gates reject it.
    let schema = make_schema(&[("x", DataType::Int32, false)]);
    let program = compile_typed("max(field(\"x\"), 127)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_does_not_saturate_float_operand() {
    // Floats carry `NaN`, which min/max propagate rather than saturate, so a float
    // operand is never collapsed regardless of the constant.
    let schema = make_schema(&[("x", DataType::Float64, false)]);
    let program = compile_typed("max(field(\"x\"), 1.0)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_keeps_saturation_when_operand_can_fail() {
    // `max(toInt8(x), 127)`: `toInt8` can fail (out-of-range), so dropping it would
    // discard a potential error — the saturation must NOT fire even though 127 is
    // `Int8::MAX` and the types match.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let program = compile_typed("max(toInt8(field(\"x\")), 127)", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "a fallible operand keeps the max, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_string_add_flattens_to_concat() {
    // Each `+` over Text fields is `add`, which the typed pass swaps to `concat`,
    // splicing the chain into one variadic concat.
    let schema = make_schema(&[
        ("a", DataType::Text { size: None }, false),
        ("b", DataType::Text { size: None }, false),
        ("c", DataType::Text { size: None }, false),
    ]);
    let program = compile_typed(
        "field(\"a\") + field(\"b\") + field(\"c\")",
        Some(&schema),
        None,
    )
    .unwrap();
    let OptNode::Call { args, .. } = program.node(program.result()) else {
        panic!(
            "expected a concat call, got {:?}",
            program.node(program.result())
        );
    };
    assert_eq!(
        program.args(*args).len(),
        3,
        "the string + chain flattens to one concat"
    );
}

#[test]
fn typed_string_add_preserves_value() {
    // `x + x` over a NULLABLE Text field becomes `concat(x, x)`, value-equal to the
    // add on both a present string AND null (both propagate null identically).
    let schema = make_schema(&[("x", DataType::Text { size: None }, true)]);
    let typed = compile_typed("field(\"x\") + field(\"x\")", Some(&schema), None).unwrap();
    let untyped = compile_typed("field(\"x\") + field(\"x\")", None, None).unwrap();
    for value in [Value::Text("hi".to_owned()), Value::Null] {
        assert_eq!(
            eval_with_field(&typed, "x", value.clone()).unwrap(),
            eval_with_field(&untyped, "x", value).unwrap(),
        );
    }
}

#[test]
fn typed_dead_branch_with_absent_field_compiles() {
    // A typed rewrite (`x * 0 → 0`) makes the condition constant; the untyped
    // const-fold + DCE then prune the dead `field("absent")` branch. The per-round
    // type-check is tolerant (only the converged tree is strictly checked), so this
    // VALID program must compile despite the not-yet-pruned absent field.
    let schema = make_schema(&[("x", DataType::Int64, false), ("a", DataType::Int64, false)]);
    let result = compile_typed(
        "if(field(\"x\") * 0 == 0, field(\"a\"), field(\"absent\"))",
        Some(&schema),
        None,
    );
    assert!(result.is_ok(), "expected ok, got {result:?}");
}

// ---- Typed algebraic identities / annihilation -----------------------------

#[test]
fn typed_strips_bitwise_identities() {
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    for source in ["field(\"x\") | 0", "field(\"x\") ^ 0", "field(\"x\") << 0"] {
        let program = compile_typed(source, Some(&schema), None).unwrap();
        assert!(
            matches!(program.node(program.result()), OptNode::SourceField(_)),
            "{source}: got {:?}",
            program.node(program.result())
        );
    }
}

#[test]
fn typed_annihilates_bitand_zero() {
    // `x & 0` over a non-null, infallible Int64 folds to the constant 0.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let program = compile_typed("field(\"x\") & 0", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Const(_)),
        "got {:?}",
        program.node(program.result())
    );
    assert_eq!(
        eval_with_field(&program, "x", Value::Int64(123)).unwrap(),
        Value::Int64(0)
    );
}

#[test]
fn typed_boolean_absorption() {
    let schema = make_schema(&[("x", DataType::Bool, false)]);
    // `x || false → x` (unit kept), `x || true → true` (absorbing).
    let unit = compile_typed("field(\"x\") || false", Some(&schema), None).unwrap();
    assert!(matches!(unit.node(unit.result()), OptNode::SourceField(_)));
    let absorb = compile_typed("field(\"x\") || true", Some(&schema), None).unwrap();
    assert!(matches!(absorb.node(absorb.result()), OptNode::Const(_)));
    assert_eq!(
        eval_with_field(&absorb, "x", Value::Bool(false)).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn typed_identity_preserves_value() {
    // The bitwise identity `x | 0 → x` is value-exact: typed and untyped agree on
    // every row.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let typed = compile_typed("field(\"x\") | 0", Some(&schema), None).unwrap();
    let untyped = compile_typed("field(\"x\") | 0", None, None).unwrap();
    for value in [Value::Int64(0), Value::Int64(42), Value::Int64(-7)] {
        assert_eq!(
            eval_with_field(&typed, "x", value.clone()).unwrap(),
            eval_with_field(&untyped, "x", value.clone()).unwrap(),
        );
    }
}

// ---- Typed power reduction -------------------------------------------------

#[test]
fn typed_reduces_unit_power_on_float() {
    let schema = make_schema(&[("x", DataType::Float64, false)]);
    // `x ** 1 → x`, value-exact on a sample row.
    let one = compile_typed("field(\"x\") ** 1", Some(&schema), None).unwrap();
    assert!(matches!(one.node(one.result()), OptNode::SourceField(_)));
    let untyped_one = compile_typed("field(\"x\") ** 1", None, None).unwrap();
    assert_eq!(
        eval_with_field(&one, "x", Value::Float64(2.5)).unwrap(),
        eval_with_field(&untyped_one, "x", Value::Float64(2.5)).unwrap(),
    );
    // `x ** 0 → 1.0`.
    let zero = compile_typed("field(\"x\") ** 0", Some(&schema), None).unwrap();
    assert!(matches!(zero.node(zero.result()), OptNode::Const(_)));
    assert_eq!(
        eval_with_field(&zero, "x", Value::Float64(7.0)).unwrap(),
        Value::Float64(1.0)
    );
}

#[test]
fn typed_does_not_reduce_square_power() {
    // `x ** 2` is NOT reduced to `x * x`: `powf(x, 2.0)` is not portably bit-equal
    // to `x * x` (powf is not correctly-rounded on every libm), so the rewrite
    // would silently change per-row values. It stays a `power` call.
    let schema = make_schema(&[("x", DataType::Float64, false)]);
    let program = compile_typed("field(\"x\") ** 2", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "x**2 must survive as a power call, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_keeps_pow_zero_on_nullable_base() {
    // `pow(x, 0)` drops `x`; for a NULLABLE base that is unsound (`pow(null,0)` is
    // null, not 1.0), so the call must survive.
    let schema = make_schema(&[("x", DataType::Float64, true)]);
    let program = compile_typed("field(\"x\") ** 0", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "nullable base keeps pow(x,0), got {:?}",
        program.node(program.result())
    );
    // and it still yields null on a null row, matching the unreduced semantics.
    assert_eq!(
        eval_with_field(&program, "x", Value::Null).unwrap(),
        Value::Null
    );
}

#[test]
fn typed_keeps_annihilation_on_nullable_operand() {
    // `x & 0` drops `x`; for a NULLABLE operand that is unsound (`null & 0` is
    // null, not 0), so the annihilation must NOT fire.
    let schema = make_schema(&[("x", DataType::Int64, true)]);
    let program = compile_typed("field(\"x\") & 0", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "nullable operand keeps x & 0, got {:?}",
        program.node(program.result())
    );
    assert_eq!(
        eval_with_field(&program, "x", Value::Null).unwrap(),
        Value::Null
    );
}

#[test]
fn typed_self_inverse_folds_to_zero() {
    // `x - x → 0` and `x ^ x → 0` over a non-null, infallible, pure Int64; the
    // folded constant matches the unreduced value on every row.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    for source in ["field(\"x\") - field(\"x\")", "field(\"x\") ^ field(\"x\")"] {
        let typed = compile_typed(source, Some(&schema), None).unwrap();
        assert!(
            matches!(typed.node(typed.result()), OptNode::Const(_)),
            "{source}: got {:?}",
            typed.node(typed.result())
        );
        let untyped = compile_typed(source, None, None).unwrap();
        for sample in [Value::Int64(0), Value::Int64(99), Value::Int64(-5)] {
            assert_eq!(
                eval_with_field(&typed, "x", sample.clone()).unwrap(),
                eval_with_field(&untyped, "x", sample.clone()).unwrap(),
            );
        }
    }
}

// ---- Typed self-comparison folding -----------------------------------------

#[test]
fn typed_strict_self_inequality_folds_false() {
    // `x > x` / `x < x` is false for every operand — folds to a constant false that
    // matches the unreduced comparison on every sample row.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    for source in ["field(\"x\") > field(\"x\")", "field(\"x\") < field(\"x\")"] {
        let typed = compile_typed(source, Some(&schema), None).unwrap();
        assert!(
            matches!(typed.node(typed.result()), OptNode::Const(_)),
            "{source}: got {:?}",
            typed.node(typed.result())
        );
        let untyped = compile_typed(source, None, None).unwrap();
        for sample in [Value::Int64(0), Value::Int64(42), Value::Int64(-9)] {
            assert_eq!(
                eval_with_field(&typed, "x", sample.clone()).unwrap(),
                Value::Bool(false),
            );
            assert_eq!(
                eval_with_field(&untyped, "x", sample.clone()).unwrap(),
                Value::Bool(false),
            );
        }
    }
}

#[test]
fn typed_self_equality_folds_for_non_float() {
    // `x == x → true` and `x != x → false` for a non-float operand — sound even when
    // nullable (`null == null` is true), so a nullable Int64 still folds.
    for nullable in [false, true] {
        let schema = make_schema(&[("x", DataType::Int64, nullable)]);
        let eq = compile_typed("field(\"x\") == field(\"x\")", Some(&schema), None).unwrap();
        assert!(
            matches!(eq.node(eq.result()), OptNode::Const(_)),
            "== nullable={nullable}: got {:?}",
            eq.node(eq.result())
        );
        assert_eq!(
            eval_with_field(&eq, "x", Value::Int64(7)).unwrap(),
            Value::Bool(true)
        );
        let ne = compile_typed("field(\"x\") != field(\"x\")", Some(&schema), None).unwrap();
        assert!(
            matches!(ne.node(ne.result()), OptNode::Const(_)),
            "!= nullable={nullable}: got {:?}",
            ne.node(ne.result())
        );
        assert_eq!(
            eval_with_field(&ne, "x", Value::Int64(7)).unwrap(),
            Value::Bool(false)
        );
    }
}

#[test]
fn typed_self_equality_skips_float() {
    // Float self-equality is the canonical NaN test (`NaN == NaN` is false), so it
    // must NOT fold to a constant — and it genuinely returns false for NaN.
    let schema = make_schema(&[("x", DataType::Float64, false)]);
    let program = compile_typed("field(\"x\") == field(\"x\")", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "float x == x must survive, got {:?}",
        program.node(program.result())
    );
    assert_eq!(
        eval_with_field(&program, "x", Value::Float64(f64::NAN)).unwrap(),
        Value::Bool(false),
    );
}

#[test]
fn typed_self_ordering_or_equal_requires_non_null() {
    // `x >= x` / `x <= x` return false on a null operand, so they fold to true only
    // for a NON-NULL non-float operand; a nullable operand keeps the call.
    let non_null = make_schema(&[("x", DataType::Int64, false)]);
    for source in [
        "field(\"x\") >= field(\"x\")",
        "field(\"x\") <= field(\"x\")",
    ] {
        let program = compile_typed(source, Some(&non_null), None).unwrap();
        assert!(
            matches!(program.node(program.result()), OptNode::Const(_)),
            "{source} (non-null): got {:?}",
            program.node(program.result())
        );
        assert_eq!(
            eval_with_field(&program, "x", Value::Int64(5)).unwrap(),
            Value::Bool(true)
        );
    }
    let nullable = make_schema(&[("x", DataType::Int64, true)]);
    let program = compile_typed("field(\"x\") >= field(\"x\")", Some(&nullable), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "nullable x >= x keeps the call, got {:?}",
        program.node(program.result())
    );
}

#[test]
fn typed_self_comparison_keeps_fallible_operand() {
    // `x / y > x / y`: divide can raise (division by zero), so dropping the operand
    // would drop the error — the fold must NOT fire.
    // Covers all three fold paths that share the can_fail gate: the early `>`
    // return, the non-float `==` arm, and the non-null `>=` arm.
    let schema = make_schema(&[("x", DataType::Int64, false), ("y", DataType::Int64, false)]);
    for op in [">", "==", ">="] {
        let source = format!("(field(\"x\") / field(\"y\")) {op} (field(\"x\") / field(\"y\"))");
        let program = compile_typed(&source, Some(&schema), None).unwrap();
        assert!(
            matches!(program.node(program.result()), OptNode::Call { .. }),
            "{source}: fallible operand keeps the comparison, got {:?}",
            program.node(program.result())
        );
    }
}

// ---- Typed unary integer reductions ----------------------------------------

#[test]
fn typed_is_nan_on_integer_folds_false() {
    for dt in [
        DataType::Int64,
        DataType::UInt32,
        DataType::BigInt { width: Some(40) },
    ] {
        let schema = make_schema(&[("x", dt.clone(), false)]);
        let program = compile_typed("isNaN(field(\"x\"))", Some(&schema), None).unwrap();
        assert!(
            matches!(program.node(program.result()), OptNode::Const(_)),
            "isNaN over {dt:?}: got {:?}",
            program.node(program.result())
        );
    }
    // Typed fold and the unreduced call agree (the unoptimized `isNaN(UInt32)`
    // now evaluates cleanly → false), so the rewrite is value-preserving.
    let schema = make_schema(&[("x", DataType::UInt32, false)]);
    let typed = compile_typed("isNaN(field(\"x\"))", Some(&schema), None).unwrap();
    let untyped = compile_typed("isNaN(field(\"x\"))", None, None).unwrap();
    assert_eq!(
        eval_with_field(&typed, "x", Value::UInt32(123)).unwrap(),
        eval_with_field(&untyped, "x", Value::UInt32(123)).unwrap(),
    );
    assert_eq!(
        eval_with_field(&typed, "x", Value::UInt32(123)).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn typed_is_infinite_folds_for_fixed_width_keeps_bigint() {
    // Fixed-width int → always a finite f64 → false.
    let fixed = make_schema(&[("x", DataType::Int64, false)]);
    let folded = compile_typed("isInfinite(field(\"x\"))", Some(&fixed), None).unwrap();
    assert!(
        matches!(folded.node(folded.result()), OptNode::Const(_)),
        "isInfinite(fixed int) folds, got {:?}",
        folded.node(folded.result())
    );
    // Typed fold agrees with the unreduced call (which now evaluates cleanly).
    let untyped = compile_typed("isInfinite(field(\"x\"))", None, None).unwrap();
    assert_eq!(
        eval_with_field(&folded, "x", Value::Int64(9)).unwrap(),
        eval_with_field(&untyped, "x", Value::Int64(9)).unwrap(),
    );
    assert_eq!(
        eval_with_field(&folded, "x", Value::Int64(9)).unwrap(),
        Value::Bool(false)
    );
    // BigInt can overflow f64 to infinity, so it must NOT fold.
    let big = make_schema(&[("x", DataType::BigInt { width: Some(80) }, false)]);
    let kept = compile_typed("isInfinite(field(\"x\"))", Some(&big), None).unwrap();
    assert!(
        matches!(kept.node(kept.result()), OptNode::Call { .. }),
        "isInfinite(BigInt) keeps the call, got {:?}",
        kept.node(kept.result())
    );
}

#[test]
fn typed_is_nan_isinfinite_keep_nullable_operand() {
    // isNaN/isInfinite drop their operand; `isNaN(null)` is null, not false, so a
    // nullable operand keeps the call (the shared `is_droppable` gate). Both
    // predicates share that gate, so both are pinned here.
    let schema = make_schema(&[("x", DataType::Int64, true)]);
    for source in ["isNaN(field(\"x\"))", "isInfinite(field(\"x\"))"] {
        let program = compile_typed(source, Some(&schema), None).unwrap();
        assert!(
            matches!(program.node(program.result()), OptNode::Call { .. }),
            "{source}: nullable operand keeps the call, got {:?}",
            program.node(program.result())
        );
        assert_eq!(
            eval_with_field(&program, "x", Value::Null).unwrap(),
            Value::Null
        );
    }
}

#[test]
fn typed_abs_on_unsigned_is_identity() {
    // `abs(x)` for an unsigned integer is the identity (always non-negative); it
    // strips to the bare field — and matches the UNREDUCED value on every row
    // (`abs(UInt32)` now evaluates cleanly), so the rewrite is a pure optimization,
    // not an error→value divergence. The `abs` no-null-gate is pinned via the
    // nullable case (it keeps the operand, so it strips regardless of nullability).
    for nullable in [false, true] {
        let schema = make_schema(&[("x", DataType::UInt32, nullable)]);
        let typed = compile_typed("abs(field(\"x\"))", Some(&schema), None).unwrap();
        assert!(
            matches!(typed.node(typed.result()), OptNode::SourceField(_)),
            "abs(uint) strips to the field (nullable={nullable}), got {:?}",
            typed.node(typed.result())
        );
        let untyped = compile_typed("abs(field(\"x\"))", None, None).unwrap();
        assert_eq!(
            eval_with_field(&typed, "x", Value::UInt32(42)).unwrap(),
            eval_with_field(&untyped, "x", Value::UInt32(42)).unwrap(),
            "typed and untyped abs(uint) agree (nullable={nullable})",
        );
        assert_eq!(
            eval_with_field(&typed, "x", Value::UInt32(42)).unwrap(),
            Value::UInt32(42)
        );
    }
}

#[test]
fn typed_abs_keeps_signed_operand() {
    // A signed operand can be negative, so `abs` is NOT the identity and survives.
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let program = compile_typed("abs(field(\"x\"))", Some(&schema), None).unwrap();
    assert!(
        matches!(program.node(program.result()), OptNode::Call { .. }),
        "abs(signed) survives, got {:?}",
        program.node(program.result())
    );
    assert_eq!(
        eval_with_field(&program, "x", Value::Int64(-5)).unwrap(),
        Value::Int64(5)
    );
}

#[test]
fn sqrt_of_constant_folds_to_value() {
    // sqrt is pure and infallible at eval (negative → NaN, never an error), so a
    // constant argument already const-folds in the untyped pass.
    for (source, want) in [("sqrt(1)", 1.0_f64), ("sqrt(0)", 0.0_f64)] {
        let program = compile_typed(source, None, None).unwrap();
        let OptNode::Const(cid) = program.node(program.result()) else {
            panic!(
                "{source} must fold to a const, got {:?}",
                program.node(program.result())
            );
        };
        assert_eq!(program.constant(*cid), &Value::Float64(want), "{source}");
    }
}

#[test]
fn function_type_mismatch_surfaces_as_a_compile_error() {
    // The headline of the pass: a `Call` whose args are all known is fed to the
    // function's `resolve_type`, and a `TypeMismatch` (here `add` over a Text
    // field + an int) must surface as a compile error — not be swallowed into an
    // unknown/Ok result. `resolve_type`'s `FuncError` maps to `OptimizeError`
    // via `#[from]`, so it lands as the `Function` variant.
    let schema = make_schema(&[("s", DataType::Text { size: None }, false)]);
    let err = compile_typed("field(\"s\") + 1", Some(&schema), None).unwrap_err();
    assert!(matches!(err, OptimizeError::Function(_)), "got {err:?}");
}

#[test]
fn conditional_result_type_flows_to_the_output_check() {
    // An `if` synthesizes its result type from the branches (merge_branches):
    // both branches are the Int64 field, so the result is Int64 and a narrowing
    // Int8 sink is rejected without truncate — proving the branch type actually
    // flowed through the conditional into the output-compat check.
    let schema = make_schema(&[("c", DataType::Bool, false), ("x", DataType::Int64, false)]);
    let expected = ExpectedOutput {
        data_type: DataType::Int8,
        truncate: false,
    };
    let err = compile_typed(
        "if(field(\"c\"), field(\"x\"), field(\"x\"))",
        Some(&schema),
        Some(&expected),
    )
    .unwrap_err();
    assert!(
        matches!(err, OptimizeError::OutputTypeMismatch { .. }),
        "got {err:?}"
    );
}

#[test]
fn schemaless_with_sample_absent_field_is_unknown_not_an_error() {
    // A sample-derived schema is not authoritative: a field absent from the
    // sample may still exist on a row, so it types as unknown (no
    // FieldNotInSchema) — unlike the fixed-schema case.
    let sampled = Schema::schemaless_with_sample(vec![Field {
        name: "seen".to_owned(),
        data_type: DataType::Int64,
        nullable: false,
    }]);
    let result = compile_typed("field(\"absent\")", Some(&sampled), None);
    assert!(result.is_ok(), "got {result:?}");
}

// ---------------------------------------------------------------------------
// Scoped-binding blocks (`if (c) { x = e; …; result } else …`).
//
// Phase C: the converter is the first real producer of `OptExpr::Block`, so
// every pass arm that was wired ahead of time is exercised here against actual
// block-carrying programs — lowering/scoping, rewrite rules inside blocks, the
// second-pass inliner/pruner, dce, compaction to `Bind`, lazy evaluation, and
// the switch-lowering no-clone guard.
// ---------------------------------------------------------------------------

#[test]
fn new_if_else_chain_lowers_identically_to_legacy() {
    // The brace-free `if (c) v else if …` chain is pure syntax: after
    // optimization it is structurally identical to the legacy `if(c, v, …)`
    // function form.
    assert_eq!(
        optimized_result("if (field(\"c1\")) 1 else if (field(\"c2\")) 2 else 3"),
        optimized_result("if(field(\"c1\"), 1, if(field(\"c2\"), 2, 3))"),
    );
}

#[test]
fn block_lowers_to_a_block_node_with_a_fresh_register() {
    let registry = registry();
    let context = context();
    let program = optimize(
        &parse(r#"if (field("c")) { x = field("a"); x } else 0"#),
        &registry,
        &context,
        false,
    )
    .unwrap();
    assert_eq!(
        program.register_count, 1,
        "the block binding takes a register"
    );
    let OptExpr::If { then_branch, .. } = program.result else {
        panic!("expected an if, got {:?}", program.result);
    };
    let OptExpr::Block {
        statements, result, ..
    } = *then_branch
    else {
        panic!("expected a Block branch, got {then_branch:?}");
    };
    assert_eq!(statements.len(), 1);
    assert_eq!(
        *result,
        OptExpr::Register(NodeId::PLACEHOLDER, statements[0].register),
        "the block result reads its own binding"
    );
}

#[test]
fn block_binding_is_not_visible_after_the_closing_brace() {
    // `q` is block-scoped: the converter restores the name map on block exit,
    // so the trailing read is an undefined variable.
    let registry = registry();
    let context = context();
    let program = parse("y = if (true) { q = 1; q } else 0; q");
    let result = optimize(&program, &registry, &context, false);
    assert!(
        matches!(result, Err(OptimizeError::UndefinedVariable { ref name }) if name == "q"),
        "expected UndefinedVariable for q, got {result:?}"
    );
}

#[test]
fn block_shadowing_leaves_the_outer_binding_untouched() {
    // The block rebinds `x` locally; the outer `x` is untouched afterwards.
    let source = "x = 1; y = if (true) { x = 2; x + 10 } else 0; x + y";
    assert_eq!(eval_unoptimized(source), Value::Int64(13));
    assert_eq!(
        optimized_result(source),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(13))
    );
}

#[test]
fn sibling_blocks_shadow_the_same_name_independently() {
    let source = "x = 5; a = if (false) { x = 1; x } else { x = 2; x }; x + a";
    assert_eq!(eval_unoptimized(source), Value::Int64(7));
    assert_eq!(
        optimized_result(source),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(7))
    );
}

#[test]
fn nested_blocks_shadow_and_restore_in_order() {
    // The inner block rebinds `a`; the outer block's `a` is restored after the
    // inner one closes: a=1, inner a=2 → b=20, outer a+b = 21.
    let source = "if (true) { a = 1; b = if (true) { a = a + 1; a * 10 } else 0; a + b } else 0";
    assert_eq!(eval_unoptimized(source), Value::Int64(21));
    assert_eq!(
        optimized_result(source),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(21))
    );
}

#[test]
fn bind_in_an_untaken_branch_never_evaluates() {
    // The unoptimized path keeps the conditional and the block: the compacted
    // `Bind` for `x = 1 / 0` sits in the untaken else branch and must never
    // run — block bindings are lazy, scoped to their branch.
    let registry = registry();
    let context = context();
    let program = compile_unoptimized("if (true) 1 else { x = 1 / 0; x }");
    let value = eval_const_program(&program, &registry, &context).unwrap();
    assert_eq!(value, Value::Int64(1));
}

#[test]
fn bind_in_the_taken_branch_executes_and_is_read_twice() {
    assert_eq!(
        eval_unoptimized("if (false) 0 else { x = 7; x + x }"),
        Value::Int64(14)
    );
}

#[test]
fn const_folding_fires_inside_a_block_binding() {
    // `1 + 2` folds within the block; the constant binding then inlines and the
    // emptied block collapses to its result.
    assert_eq!(
        optimized_result(r#"if (field("c")) { x = 1 + 2; x } else 0"#),
        optimized_result(r#"if(field("c"), 3, 0)"#),
    );
}

#[test]
fn constant_block_binding_inlines_into_its_scope() {
    // `x = 3` inlines into both reads, `3 + 3` folds, the block collapses.
    assert_eq!(
        optimized_result(r#"if (field("c")) { x = 3; x + x } else 0"#),
        optimized_result(r#"if(field("c"), 6, 0)"#),
    );
}

#[test]
fn prunes_unused_infallible_block_binding_and_collapses_the_block() {
    // The unread `x` is infallible (`add`), so the block binding is dropped and
    // the empty block collapses to its result.
    assert_eq!(
        optimized_result(r#"if (field("c")) { x = field("a") + 1; 2 } else 0"#),
        optimized_result(r#"if(field("c"), 2, 0)"#),
    );
}

#[test]
fn keeps_unused_fallible_block_binding() {
    // `divide` may fail; the block evaluates its bindings when reached, so the
    // unread binding must survive to preserve the (lazy) error.
    let result = optimized_result(r#"if (field("c")) { x = field("a") / field("b"); 2 } else 0"#);
    let OptExpr::If { then_branch, .. } = result else {
        panic!("expected an if, got {result:?}");
    };
    let OptExpr::Block { statements, .. } = *then_branch else {
        panic!("expected a surviving Block, got {then_branch:?}");
    };
    assert_eq!(statements.len(), 1, "the fallible binding is kept");
}

#[test]
fn branch_prune_keeps_the_taken_block_wholesale() {
    // `if (true)` resolves to the then-block; the block (with its fallible,
    // read binding) becomes the whole result.
    let result =
        optimized_result(r#"if (true) { x = field("a") / field("b"); x } else field("z")"#);
    let OptExpr::Block {
        statements, result, ..
    } = result
    else {
        panic!("expected the taken Block, got {result:?}");
    };
    assert_eq!(statements.len(), 1);
    assert_eq!(
        *result,
        OptExpr::Register(NodeId::PLACEHOLDER, statements[0].register)
    );
}

#[test]
fn branch_prune_discards_the_untaken_block_with_its_bindings() {
    // The untaken block — including its erroring constant binding — is dropped
    // wholesale; no ConstEval error escapes a dead branch.
    assert_eq!(
        optimized_result("if (false) { x = 1 / 0; x } else 7"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Int64(7))
    );
}

#[test]
fn eager_block_binding_with_constant_error_fails_the_build() {
    // `if (true)` makes the block always-reached, so its erroring constant
    // binding is an eager position — the static check rejects it, mirroring the
    // top-level `x = 1 / 0; 5` behaviour.
    let registry = registry();
    let context = context();
    let program = parse("if (true) { x = 1 / 0; x } else 0");
    let result = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn lazy_block_binding_with_constant_error_compiles() {
    // Behind a dynamic condition the block stays lazy, so the erroring constant
    // binding defers to runtime.
    let registry = registry();
    let context = context();
    let program = parse(r#"if (field("c")) { x = 1 / 0; x } else 0"#);
    let compiled = Optimizer::create(&registry, &context).compile(&program, None, None);
    assert!(compiled.is_ok(), "got {compiled:?}");
}

#[test]
fn guard_propagation_reaches_inside_a_block() {
    // `c == "yes"` pins `c` inside the then-block, so `upper(c)` folds to
    // "YES"; the constant binding inlines and the block collapses.
    let result =
        optimized_result(r#"if (field("c") == "yes") { v = upper(field("c")); v } else "no""#);
    let OptExpr::If { then_branch, .. } = result else {
        panic!("expected an if, got {result:?}");
    };
    assert_eq!(
        *then_branch,
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Text("YES".to_string()))
    );
}

#[test]
fn switch_lowering_bails_on_a_block_branch_value() {
    // Cloning a branch value into the dispatch table would alias a block's
    // register writes, so a block-carrying branch keeps the multiIf intact…
    let mut block_branch = String::from("multiIf(");
    let mut plain_branch = String::from("multiIf(");
    let block_value = r#"if (field("c")) { v = field("y") + 1; v * v } else 0"#;
    for index in 1..=6 {
        let value = if index == 1 {
            block_value.to_string()
        } else {
            format!("{index}0")
        };
        block_branch.push_str(&format!(r#"field("x") == {index}, {value}, "#));
        plain_branch.push_str(&format!(r#"field("x") == {index}, {index}0, "#));
    }
    block_branch.push_str("0)");
    plain_branch.push_str("0)");

    match optimized_result(&block_branch) {
        OptExpr::MultiIf { branches, .. } => assert_eq!(branches.len(), 6),
        other => panic!("expected the multiIf kept, got {other:?}"),
    }
    // …while the identical shape without the block still lowers to a Switch.
    assert!(matches!(
        optimized_result(&plain_branch),
        OptExpr::Switch { .. }
    ));
}

#[test]
fn short_circuit_fold_keeps_the_bool_check_on_the_surviving_operand() {
    // `false || x` / `true && x` make the right operand the whole result, but
    // the evaluator would still have required it to be Bool/Null. The fold
    // must preserve that as a TypeAssert{Bool} when the operand's type is not
    // provable, and the typed engine strips it when it is.
    let asserted = optimized_result(r#"false || field("x")"#);
    assert!(
        matches!(
            asserted,
            OptExpr::TypeAssert {
                expect: TypeClass::Bool,
                ..
            }
        ),
        "expected a Bool TypeAssert, got {asserted:?}"
    );
    let and_asserted = optimized_result(r#"true && field("x")"#);
    assert!(matches!(
        and_asserted,
        OptExpr::TypeAssert {
            expect: TypeClass::Bool,
            ..
        }
    ));
    // The typed engine (schema-present compiles only — `Optimizer::optimize`
    // builds the typed rule set off `schema.map(...)`) strips the assert when
    // the operand is provably Bool…
    let schema = make_schema(&[("x", DataType::Int64, false)]);
    let stripped = Optimizer::create(&registry(), &context())
        .optimize(&parse(r#"false || (field("x") == 1)"#), true, Some(&schema))
        .unwrap()
        .result;
    assert!(
        !matches!(stripped, OptExpr::TypeAssert { .. }),
        "expected the assert stripped over a comparison, got {stripped:?}"
    );
    // …and a constant Bool right operand folds bare.
    assert_eq!(
        optimized_result("false || true"),
        OptExpr::Const(NodeId::PLACEHOLDER, Value::Bool(true))
    );

    // Runtime behavior matches the heap evaluator: Bool passes, non-Bool errors.
    let program = compile_optimized(r#"false || field("x")"#);
    assert_eq!(
        eval_with_field(&program, "x", Value::Bool(true)).unwrap(),
        Value::Bool(true)
    );
    assert!(eval_with_field(&program, "x", Value::Int64(5)).is_err());
}

#[test]
fn switch_lowering_bails_on_a_block_key_expression() {
    // A block inside the *key* expression must also keep the multiIf intact:
    // the surviving key is a clone, and cloning a block would alias its
    // register writes. Today the cross-clause structural-equality check already
    // rejects blocks (fresh SSA registers never compare equal), but this test
    // pins the outcome itself, not the mechanism — see the explicit
    // `contains_block` guard on `key_exprs` in `switch_lower.rs`.
    let key = r#"if (field("d")) { v = field("k"); v } else field("k")"#;
    let mut block_key = String::from("multiIf(");
    let mut plain_key = String::from("multiIf(");
    for index in 1..=6 {
        block_key.push_str(&format!("{key} == {index}, {index}0, "));
        plain_key.push_str(&format!(r#"field("k") == {index}, {index}0, "#));
    }
    block_key.push_str("0)");
    plain_key.push_str("0)");

    match optimized_result(&block_key) {
        OptExpr::MultiIf { branches, .. } => assert_eq!(branches.len(), 6),
        other => panic!("expected the multiIf kept, got {other:?}"),
    }
    assert!(matches!(
        optimized_result(&plain_key),
        OptExpr::Switch { .. }
    ));
}

#[test]
fn compiled_impure_block_binding_evaluates_once() {
    // The evaluate-once contract on the *compiled* path: the binding writes its
    // register once per evaluation, so both reads observe the same draw. A
    // broken implementation that re-evaluates the value per read would make
    // `v == v` flaky (two independent draws collide with p = 1e-6).
    let source = r#"if (field("c")) { v = randomInt(0, 1000000); v == v } else false"#;

    // Guard against this test going vacuous: no rule folds `v == v` today, so
    // the block must survive optimization. A future value-numbering identity
    // (sound for non-float types only — NaN != NaN) would collapse it to
    // `true` and prune the binding; if that lands, rework this test instead of
    // letting it silently assert a constant.
    match optimized_result(source) {
        OptExpr::If { then_branch, .. } => {
            assert!(matches!(*then_branch, OptExpr::Block { .. }))
        }
        other => panic!("expected an if with a surviving block, got {other:?}"),
    }

    let program = compile_optimized(source);
    assert_eq!(
        eval_with_field(&program, "c", Value::Bool(true)).unwrap(),
        Value::Bool(true)
    );
}

#[test]
fn optimized_block_program_evaluates_end_to_end() {
    // Full pipeline over a surviving block: field-read hoisting, move
    // annotation across `Bind`, arena evaluation. `field("x")` is read twice,
    // so it hoists into a register feeding the block.
    let program =
        compile_optimized(r#"if (field("x") == 1) { v = field("x") + 10; v * v } else 0"#);
    assert_eq!(
        eval_with_field(&program, "x", Value::Int64(1)).unwrap(),
        Value::Int64(121)
    );
    assert_eq!(
        eval_with_field(&program, "x", Value::Int64(2)).unwrap(),
        Value::Int64(0)
    );
}

#[test]
fn optimized_block_stays_lazy_at_runtime() {
    // The fallible binding survives optimization inside the else-block and runs
    // only when that branch is taken: x == 1 short-circuits past it, x == 0
    // reaches it and raises the division error.
    let program = compile_optimized(r#"if (field("x") == 1) 1 else { d = 1 / field("x"); d }"#);
    assert_eq!(
        eval_with_field(&program, "x", Value::Int64(1)).unwrap(),
        Value::Int64(1)
    );
    assert!(eval_with_field(&program, "x", Value::Int64(0)).is_err());
}

#[test]
fn block_purity_and_fallibility_follow_bindings_and_result() {
    let registry = registry();
    let context = context();
    let lower = |source: &str| {
        let program = optimize(&parse(source), &registry, &context, false).unwrap();
        let OptExpr::If { then_branch, .. } = program.result else {
            panic!("expected an if, got {:?}", program.result);
        };
        *then_branch
    };

    // A fallible binding makes the whole block fallible; `divide` is still pure.
    let fallible = lower(r#"if (field("c")) { x = 1 / 2; x } else 0"#);
    assert!(matches!(fallible, OptExpr::Block { .. }));
    assert!(crate::util::fallibility::can_fail(&fallible, &registry));
    assert!(crate::util::type_utils::is_pure(&fallible, &registry));

    // An impure binding (random) makes the block impure.
    let impure = lower(r#"if (field("c")) { x = randomInt(0, 10); x } else 0"#);
    assert!(matches!(impure, OptExpr::Block { .. }));
    assert!(!crate::util::type_utils::is_pure(&impure, &registry));

    // Pure, infallible bindings + result: the block is pure and infallible.
    let benign = lower(r#"if (field("c")) { x = field("a") + 1; x } else 0"#);
    assert!(matches!(benign, OptExpr::Block { .. }));
    assert!(!crate::util::fallibility::can_fail(&benign, &registry));
    assert!(crate::util::type_utils::is_pure(&benign, &registry));
}

#[test]
fn typed_path_types_block_programs() {
    // The type checker threads block bindings through the register table: a
    // well-typed block program compiles, and a type error inside a block is
    // raised by the final strict pass even in a lazy branch.
    let schema = make_schema(&[("c", DataType::Bool, false), ("n", DataType::Int32, false)]);
    let ok = compile_typed(
        r#"if (field("c")) { x = field("n") + 1; x + x } else 0"#,
        Some(&schema),
        None,
    );
    assert!(ok.is_ok(), "got {ok:?}");

    let bad = compile_typed(
        r#"if (field("c")) { x = trim(field("n")); x } else 'a'"#,
        Some(&schema),
        None,
    );
    assert!(bad.is_err(), "trim(Int32) inside a block must be rejected");
}
