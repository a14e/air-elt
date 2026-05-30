//! Unused-register elimination: drop statements whose register is never read.
//!
//! Pruning is iterated to a fixpoint — removing one statement can make the
//! registers *it* read unused in turn. A size-non-increasing program pass.
//!
//! A binding is dropped only when it is both unread **and** infallible. An
//! unread binding whose value may error (e.g. `x = a / b`) is kept: statements
//! are evaluated eagerly, so removing it would discard an error that the
//! unoptimized program raises. Failability is read from
//! [`ExprFunction::can_fail`](air_elt_expr_funcs::ExprFunction::can_fail) — a
//! conservative, argument-independent over-approximation (the type-aware pass
//! can refine it later).

use ahash::AHashSet;
use air_elt_expr_funcs::FunctionRegistry;

use super::engine::ProgramPass;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::OptProgram;

pub(crate) struct RegisterPruner;

impl ProgramPass for RegisterPruner {
    fn run(&self, program: &mut OptProgram, registry: &FunctionRegistry) {
        loop {
            let mut used: AHashSet<u16> = AHashSet::new();
            collect(&program.result, &mut used);
            for statement in &program.statements {
                collect(&statement.value, &mut used);
            }

            let before = program.statements.len();
            program.statements.retain(|statement| {
                used.contains(&statement.register) || can_fail(&statement.value, registry)
            });
            if program.statements.len() == before {
                break;
            }
        }
    }
}

/// Whether evaluating `expr` may raise an error. Conservative: any fallible
/// function call anywhere in the subtree, or an interpolation (which can
/// exceed the string-size cap), makes the whole expression fallible.
fn can_fail(expr: &OptExpr, registry: &FunctionRegistry) -> bool {
    match expr {
        OptExpr::Const(_) | OptExpr::Register(_) | OptExpr::SourceField(_) | OptExpr::Fields(_) => {
            false
        }
        OptExpr::Field(inner) => can_fail(inner, registry),
        OptExpr::Call { func, args } => {
            registry.get_by_ref(*func).can_fail() || args.iter().any(|arg| can_fail(arg, registry))
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            can_fail(condition, registry)
                || can_fail(then_branch, registry)
                || can_fail(else_branch, registry)
        }
        OptExpr::MultiIf { branches, default } => {
            branches.iter().any(|(condition, value)| {
                can_fail(condition, registry) || can_fail(value, registry)
            }) || can_fail(default, registry)
        }
        OptExpr::IfNull { value, alternative } => {
            can_fail(value, registry) || can_fail(alternative, registry)
        }
        OptExpr::NullIf { value, sentinel } => {
            can_fail(value, registry) || can_fail(sentinel, registry)
        }
        OptExpr::And { left, right } | OptExpr::Or { left, right } => {
            can_fail(left, registry) || can_fail(right, registry)
        }
        // Interpolation can raise `StringTooLarge` even with infallible segments.
        OptExpr::Interpolation(_) => true,
        OptExpr::Object(entries) => entries.iter().any(|(_, value)| can_fail(value, registry)),
        OptExpr::Switch {
            inputs,
            table,
            default,
        } => {
            inputs.iter().any(|input| can_fail(input, registry))
                || table.iter().any(|(_, value)| can_fail(value, registry))
                || can_fail(default, registry)
        }
        // A TypeAssert exists precisely to raise the preserved `TypeMismatch` on a
        // present, wrong-typed operand — so it can always fail.
        OptExpr::TypeAssert { .. } => true,
    }
}

fn collect(expr: &OptExpr, used: &mut AHashSet<u16>) {
    match expr {
        OptExpr::Register(register) => {
            used.insert(*register);
        }
        OptExpr::Const(_) | OptExpr::SourceField(_) | OptExpr::Fields(_) => {}
        OptExpr::Field(inner) => collect(inner, used),
        OptExpr::Call { args, .. } => {
            for arg in args {
                collect(arg, used);
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            collect(condition, used);
            collect(then_branch, used);
            collect(else_branch, used);
        }
        OptExpr::MultiIf { branches, default } => {
            for (condition, value) in branches {
                collect(condition, used);
                collect(value, used);
            }
            collect(default, used);
        }
        OptExpr::IfNull { value, alternative } => {
            collect(value, used);
            collect(alternative, used);
        }
        OptExpr::NullIf { value, sentinel } => {
            collect(value, used);
            collect(sentinel, used);
        }
        OptExpr::And { left, right } | OptExpr::Or { left, right } => {
            collect(left, used);
            collect(right, used);
        }
        OptExpr::Interpolation(segments) => {
            for segment in segments {
                collect(segment, used);
            }
        }
        OptExpr::Object(entries) => {
            for (_, value) in entries {
                collect(value, used);
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
        } => {
            for input in inputs {
                collect(input, used);
            }
            for (_, value) in table {
                collect(value, used);
            }
            collect(default, used);
        }
        OptExpr::TypeAssert { inner, .. } => collect(inner, used),
    }
}
