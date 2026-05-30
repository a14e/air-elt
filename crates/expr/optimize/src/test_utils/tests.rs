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
use air_elt_types::{Key, Value};
use proptest::prelude::*;

use crate::engines::{Compactor, EvalError, ProgramEvaluator};
use crate::error::OptimizeError;
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::opt_program::{OptProgram, OptStatement};
use crate::model::{CompactProgram, NodeRef, OptNode, RegisterId, TypeClass};
use crate::optimizer::Optimizer;

// Thin wrappers exercising the OOP API once, so the test bodies stay terse.

fn optimize(
    program: &Program,
    registry: &FunctionRegistry,
    context: &EvalContext,
    apply_rules: bool,
) -> Result<OptProgram, OptimizeError> {
    Optimizer::create(registry, context).optimize(program, apply_rules)
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
        .compile(&parse(source))
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
    assert_eq!(*then_branch, OptExpr::Const(Value::Null));
    // The else only knows `x` is non-null (no value), so the read survives —
    // here a hoisted register, since the field is read several times.
    assert!(
        matches!(*else_branch, OptExpr::Register(_) | OptExpr::SourceField(_)),
        "expected the operand read to survive, got {else_branch:?}"
    );
}

#[test]
fn substitutes_string_equality_operand_in_then_branch() {
    // `x == "yes"` pins `x` to the string, so `upper(x)` folds to "YES".
    let result = optimized_result(r#"if(field("c") == "yes", upper(field("c")), field("c"))"#);
    let OptExpr::If { then_branch, .. } = result else {
        panic!("expected if, got {result:?}");
    };
    assert_eq!(*then_branch, OptExpr::Const(Value::Text("YES".to_string())));
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
    assert_eq!(*right, OptExpr::Const(Value::Bool(false)));
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
    assert_eq!(optimized_result("1 + 2"), OptExpr::Const(Value::Int64(3)));
    assert_eq!(
        optimized_result("2 * 3 + 4"),
        OptExpr::Const(Value::Int64(10))
    );
}

#[test]
fn folds_nested_pure_calls() {
    assert_eq!(
        optimized_result("concat(toString(1 + 2), '!')"),
        OptExpr::Const(Value::Text("3!".to_string()))
    );
}

#[test]
fn collapses_field_forms_to_source_field() {
    let expected = OptExpr::SourceField("x".to_string());
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
        } => {
            assert!(matches!(*inner, OptExpr::SourceField(name) if name == "flag"));
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
        OptExpr::Const(Value::Int64(2))
    );
}

#[test]
fn prunes_constant_and_or() {
    assert_eq!(
        optimized_result("true && field(\"b\")"),
        OptExpr::SourceField("b".to_string())
    );
    assert_eq!(
        optimized_result("false && field(\"b\")"),
        OptExpr::Const(Value::Bool(false))
    );
    assert_eq!(
        optimized_result("true || field(\"b\")"),
        OptExpr::Const(Value::Bool(true))
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
    assert_eq!(folded, OptExpr::Const(Value::Text("a2b".to_string())));

    assert!(matches!(
        optimized_result("{\"k\" = 1 + 1}"),
        OptExpr::Const(Value::Json(_))
    ));
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
    let result = Optimizer::create(&registry, &context).compile(&program);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn keeps_constant_error_in_a_dead_branch() {
    // The erroring `1 / 0` is in an unreachable branch (`if(false, …)`), so the
    // optimizer must NOT fail the build — dce drops the dead branch.
    let registry = registry();
    let context = context();
    let program = parse("if(1 < 0, 1 / 0, 7)");
    let compiled = Optimizer::create(&registry, &context).compile(&program);
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
    let result = Optimizer::create(&registry, &context).compile(&program);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn keeps_invalid_constant_regex_in_a_dead_branch() {
    // The same invalid regex inside an unreachable branch must NOT fail the
    // build — dce drops the branch before it can reach runtime.
    let registry = registry();
    let context = context();
    let program = parse("if(1 < 0, regexMatch(\"abc\", \"[\"), false)");
    let compiled = Optimizer::create(&registry, &context).compile(&program);
    assert!(compiled.is_ok());
}

#[test]
fn defers_division_by_zero_with_dynamic_operand_to_runtime() {
    // `field("a") / 0` has a non-constant operand, so it cannot be folded: the
    // program compiles cleanly and the division error surfaces only at runtime.
    let registry = registry();
    let context = context();
    let program = parse("x = field(\"a\") / 0; x");
    let compiled = Optimizer::create(&registry, &context).compile(&program);
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
    let result = Optimizer::create(&registry, &context).compile(&program);
    assert!(matches!(result, Err(OptimizeError::InvalidConstArg { .. })));
}

#[test]
fn rejects_invalid_const_jspath_with_dynamic_json() {
    let registry = registry();
    let context = context();
    let program = parse("jsPath(field(\"doc\"), \"$[\")");
    let result = Optimizer::create(&registry, &context).compile(&program);
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
    let result = Optimizer::create(&registry, &context).compile(&program);
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
    let result = Optimizer::create(&registry, &context).compile(&program);
    assert!(matches!(result, Err(OptimizeError::NonConstFieldArg)));
}

#[test]
fn defers_value_failure_in_lazy_and_right_operand() {
    // `&&` right operand is lazy: a dynamic left keeps the `&&` unfolded, so the
    // constant `1 / 0` on the right defers to runtime — compilation succeeds.
    let registry = registry();
    let context = context();
    let program = parse("field(\"c\") && ((1 / 0) == 0)");
    let compiled = Optimizer::create(&registry, &context).compile(&program);
    assert!(compiled.is_ok());
}

#[test]
fn rejects_value_failure_in_eager_and_left_operand() {
    // `&&` left operand is always evaluated → the constant `1 / 0` fails the build.
    let registry = registry();
    let context = context();
    let program = parse("((1 / 0) == 0) && field(\"c\")");
    let result = Optimizer::create(&registry, &context).compile(&program);
    assert!(matches!(result, Err(OptimizeError::ConstEval { .. })));
}

#[test]
fn defers_value_failure_in_if_null_alternative() {
    // The `ifNull` alternative is lazy (reached only when the value is null).
    let registry = registry();
    let context = context();
    let program = parse("ifNull(field(\"x\"), 1 / 0)");
    let compiled = Optimizer::create(&registry, &context).compile(&program);
    assert!(compiled.is_ok());
}

#[test]
fn rejects_value_failure_in_null_if_operand() {
    // `nullIf` evaluates both operands unconditionally → eager → fails the build.
    let registry = registry();
    let context = context();
    let program = parse("nullIf(field(\"x\"), 1 / 0)");
    let result = Optimizer::create(&registry, &context).compile(&program);
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
        Some(OptExpr::SourceField(name)) => Some(name.as_str()),
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
        } => {
            assert_eq!(equals_field_name(&condition), Some("a"));
            assert_eq!(
                *then_branch,
                OptExpr::Const(Value::Int64(10)),
                "branch value"
            );
            assert_eq!(
                *else_branch,
                OptExpr::Const(Value::Int64(0)),
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
        } => {
            assert_eq!(equals_field_name(&condition), Some("a"));
            assert_eq!(
                *then_branch,
                OptExpr::Const(Value::Int64(10)),
                "first value"
            );
            match *else_branch {
                OptExpr::If {
                    condition: inner_condition,
                    then_branch: inner_then,
                    else_branch: inner_else,
                } => {
                    assert_eq!(equals_field_name(&inner_condition), Some("b"));
                    assert_eq!(
                        *inner_then,
                        OptExpr::Const(Value::Int64(20)),
                        "second value"
                    );
                    assert_eq!(
                        *inner_else,
                        OptExpr::Const(Value::Int64(0)),
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
        func: not,
        args: vec![OptExpr::Or {
            left: Box::new(OptExpr::SourceField("a".to_string())),
            right: Box::new(OptExpr::SourceField("b".to_string())),
        }],
    };
    assert_eq!(optimized_result("!field(\"a\") && !field(\"b\")"), expected);
}

#[test]
fn factors_disjunction_of_negations() {
    // not(a) || not(b) → not(a && b).
    let not = registry().get_ref("not", Some(1)).unwrap();
    let expected = OptExpr::Call {
        func: not,
        args: vec![OptExpr::And {
            left: Box::new(OptExpr::SourceField("a".to_string())),
            right: Box::new(OptExpr::SourceField("b".to_string())),
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
            assert!(matches!(*right, OptExpr::SourceField(name) if name == "b"));
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
            assert!(matches!(*left, OptExpr::SourceField(name) if name == "a"));
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
            } => {
                assert!(matches!(*inner, OptExpr::SourceField(name) if name == "x"));
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
        } => {
            assert!(matches!(*inner, OptExpr::SourceField(name) if name == "x"));
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
            inner: Box::new(OptExpr::Const(operand)),
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
            } => {
                assert!(matches!(*inner, OptExpr::SourceField(name) if name == "x"));
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
                assert!(matches!(&inner[0], OptExpr::SourceField(name) if name == "x"));
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
                    matches!(&args[0], OptExpr::SourceField(name) if name == "x"),
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
        matches!(&program.statements[0].value, OptExpr::SourceField(name) if name == "a"),
        "expected a hoisted source-field binding"
    );
    let register = program.statements[0].register;
    match &program.result {
        OptExpr::Call { args, .. } => {
            assert_eq!(args.len(), 2);
            assert!(
                args.iter()
                    .all(|arg| matches!(arg, OptExpr::Register(reg) if *reg == register)),
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
        OptExpr::Const(Value::Bool(false))
    );
    assert_eq!(
        optimized_result("null && true"),
        OptExpr::Const(Value::Null)
    );
    assert_eq!(
        optimized_result("null || true"),
        OptExpr::Const(Value::Bool(true))
    );
    assert_eq!(
        optimized_result("null || false"),
        OptExpr::Const(Value::Null)
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
        OptExpr::Const(Value::Int64(42))
    );
    assert_eq!(
        optimized_result("ifNull(7, field(\"x\"))"),
        OptExpr::Const(Value::Int64(7))
    );
    assert_eq!(
        optimized_result("nullIf(5, 5)"),
        OptExpr::Const(Value::Null)
    );
    assert_eq!(
        optimized_result("nullIf(5, 6)"),
        OptExpr::Const(Value::Int64(5))
    );
}

#[test]
fn prunes_multi_if_all_false_to_default() {
    assert_eq!(
        optimized_result("multiIf(false, 1, false, 2, 3)"),
        OptExpr::Const(Value::Int64(3))
    );
}

// ---- Arena layout: interpolation & object ---------------------------------

#[test]
fn evaluates_object_via_arena() {
    // Unoptimized keeps the Object node, exercising the key-table + value-run
    // arena path in the evaluator.
    let value = eval_unoptimized("{\"a\" = 1, \"b\" = 1 + 1}");
    match value {
        Value::Json(json) => {
            assert_eq!(json.get("a"), Some(&serde_json::json!(1)));
            assert_eq!(json.get("b"), Some(&serde_json::json!(2)));
        }
        other => panic!("expected Json, got {other:?}"),
    }
}

// ---- Compaction: constant interning ---------------------------------------

#[test]
fn compaction_dedups_type_exact_constants() {
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(vec![
            ("a".to_string(), OptExpr::Const(Value::Int64(5))),
            ("b".to_string(), OptExpr::Const(Value::Int64(5))),
        ]),
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
        result: OptExpr::Object(vec![
            ("a".to_string(), OptExpr::Const(Value::Int8(5))),
            ("b".to_string(), OptExpr::Const(Value::Int64(5))),
        ]),
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
        result: OptExpr::Object(vec![
            ("a".to_string(), OptExpr::Const(Value::Float64(0.0))),
            ("b".to_string(), OptExpr::Const(Value::Float64(-0.0))),
            ("c".to_string(), OptExpr::Const(Value::Float64(0.0))),
        ]),
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
        result: OptExpr::Object(vec![
            (
                "a".to_string(),
                OptExpr::Const(Value::Decimal(BigDecimal::from_str("1.0").unwrap())),
            ),
            (
                "b".to_string(),
                OptExpr::Const(Value::Decimal(BigDecimal::from_str("1.00").unwrap())),
            ),
        ]),
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
        .map(|n| (format!("k{n}"), OptExpr::Const(Value::Int64(n))))
        .collect();
    entries.push(("dup".to_string(), OptExpr::Const(Value::Int64(0))));
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(entries),
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
        result: OptExpr::Object(vec![
            ("a".to_string(), OptExpr::Const(Value::Int64(5))),
            ("b".to_string(), OptExpr::Const(Value::Int64(6))),
        ]),
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
        result: OptExpr::Object(vec![
            ("a".to_string(), OptExpr::Const(Value::Ipv4(v4))),
            ("b".to_string(), OptExpr::Const(Value::Ipv6(mapped))),
        ]),
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
        result: OptExpr::Object(vec![
            ("a".to_string(), OptExpr::Const(Value::Json(json.clone()))),
            ("b".to_string(), OptExpr::Const(Value::Json(json))),
        ]),
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
        OptExpr::Object(vec![
            ("id".to_string(), OptExpr::Const(Value::Int64(id))),
            (
                "name".to_string(),
                OptExpr::Const(Value::Text(name.to_string())),
            ),
        ])
    };
    let program = OptProgram {
        statements: vec![OptStatement {
            register: 0,
            value: inner(1, "a"),
        }],
        result: OptExpr::Object(vec![
            ("first".to_string(), OptExpr::Register(0)),
            ("second".to_string(), inner(2, "b")),
        ]),
        register_count: 1,
    };
    let compiled = compact(program).unwrap();
    assert_eq!(
        compiled.key_pool_len(),
        4,
        "inner names shared across the statement and the result intern once each"
    );

    let value = eval_const_program(&compiled, &registry(), &context()).unwrap();
    let Value::Json(json) = value else {
        panic!("expected a Json object");
    };
    assert_eq!(json.pointer("/first/id"), Some(&serde_json::json!(1)));
    assert_eq!(json.pointer("/first/name"), Some(&serde_json::json!("a")));
    assert_eq!(json.pointer("/second/id"), Some(&serde_json::json!(2)));
    assert_eq!(json.pointer("/second/name"), Some(&serde_json::json!("b")));
}

#[test]
fn compaction_interns_duplicate_keys_within_one_object() {
    // A name repeated inside a single object interns to one pool entry; the run
    // still holds both occurrences (same id twice) and eval applies last-wins, as
    // `serde_json::Map::insert` overwrites.
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(vec![
            ("dup".to_string(), OptExpr::Const(Value::Int64(1))),
            ("dup".to_string(), OptExpr::Const(Value::Int64(2))),
        ]),
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
    let Value::Json(json) = value else {
        panic!("expected a Json object");
    };
    assert_eq!(
        json.get("dup"),
        Some(&serde_json::json!(2)),
        "last write wins"
    );
}

#[test]
fn compaction_preserves_object_key_run_order() {
    // Interning addresses keys by pool id, but each object's key run must keep
    // its original order and stay paired with the right value.
    let program = OptProgram {
        statements: vec![],
        result: OptExpr::Object(vec![
            ("b".to_string(), OptExpr::Const(Value::Int64(1))),
            ("a".to_string(), OptExpr::Const(Value::Int64(2))),
            ("c".to_string(), OptExpr::Const(Value::Int64(3))),
        ]),
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
    let Value::Json(json) = value else {
        panic!("expected a Json object");
    };
    assert_eq!(json.get("b"), Some(&serde_json::json!(1)));
    assert_eq!(json.get("a"), Some(&serde_json::json!(2)));
    assert_eq!(json.get("c"), Some(&serde_json::json!(3)));
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
    let result = Optimizer::create(&registry(), &context()).optimize(&program, false);
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
fn evaluates_switch_dispatch() {
    // A const-keyed switch built directly (the optimizer would const-fold one
    // away), to exercise compaction of the table + `eval_switch`.
    let table = vec![
        (
            Key::single(Value::Int64(1)).unwrap(),
            OptExpr::Const(Value::Int64(10)),
        ),
        (
            Key::single(Value::Int64(2)).unwrap(),
            OptExpr::Const(Value::Int64(20)),
        ),
        (
            Key::single(Value::Int64(3)).unwrap(),
            OptExpr::Const(Value::Int64(30)),
        ),
    ];
    let registry = registry();
    let context = context();
    let dispatch = |input: Value| {
        let program = OptProgram {
            statements: vec![],
            result: OptExpr::Switch {
                inputs: vec![OptExpr::Const(input)],
                table: table.clone(),
                default: Box::new(OptExpr::Const(Value::Int64(0))),
            },
            register_count: 0,
        };
        let compiled = Compactor::create().compact(program).unwrap();
        eval_const_program(&compiled, &registry, &context).unwrap()
    };
    assert_eq!(dispatch(Value::Int64(2)), Value::Int64(20)); // hit
    assert_eq!(dispatch(Value::Int64(99)), Value::Int64(0)); // miss → default
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
fn move_marks_last_read_of_a_hoisted_register() {
    // `field("c")` read twice → field hoist binds it to a register; the two
    // reads become `[Register(r), Register(r)]`. The annotator turns the LAST
    // read into a move and leaves the earlier one a clone.
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
        matches!(reads[1], OptNode::RegisterTake(r) if *r == register),
        "the last read moves: {:?}",
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
fn move_does_not_corrupt_an_earlier_register_read() {
    // The last read of an impure register moves the value out (leaving the slot
    // null); the earlier read cloned it first, so both halves must be equal — the
    // move must not corrupt the value the earlier read observed.
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
        matches!(reads[1], OptNode::RegisterTake(_)),
        "{:?}",
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
    // `ifNull(x, upper(x))`: `x` (always-run value) must clone because the lazy
    // alternative re-reads it; the alternative's read is the last use and moves.
    let compiled = compile_optimized(r#"x = randomHex(4); ifNull(x, upper(x))"#);
    let OptNode::IfNull { value, alternative } = compiled.node(compiled.result()) else {
        panic!("expected an ifNull result");
    };
    let value_read = compiled.node(*value);
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
        let optimized = match Optimizer::create(&registry, &context).compile(&program) {
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
