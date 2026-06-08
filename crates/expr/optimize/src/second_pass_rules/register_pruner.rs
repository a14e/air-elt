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
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::OptProgram;
use crate::util::fallibility::can_fail;

pub(crate) struct RegisterPruner;

impl ProgramPass for RegisterPruner {
    fn run(
        &self,
        program: &mut OptProgram,
        registry: &FunctionRegistry,
        _node_counter: &NodeCounter,
    ) {
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

fn collect(expr: &OptExpr, used: &mut AHashSet<u16>) {
    match expr {
        OptExpr::Register(_, register) => {
            used.insert(*register);
        }
        OptExpr::Const(..) | OptExpr::SourceField(..) | OptExpr::Fields(..) => {}
        OptExpr::Field(_, inner) => collect(inner, used),
        OptExpr::Call { args, .. } => {
            for arg in args {
                collect(arg, used);
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            collect(condition, used);
            collect(then_branch, used);
            collect(else_branch, used);
        }
        OptExpr::MultiIf {
            branches, default, ..
        } => {
            for (condition, value) in branches {
                collect(condition, used);
                collect(value, used);
            }
            collect(default, used);
        }
        OptExpr::IfNull {
            value, alternative, ..
        } => {
            collect(value, used);
            collect(alternative, used);
        }
        OptExpr::NullIf {
            value, sentinel, ..
        } => {
            collect(value, used);
            collect(sentinel, used);
        }
        OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
            collect(left, used);
            collect(right, used);
        }
        OptExpr::Interpolation(_, segments) => {
            for segment in segments {
                collect(segment, used);
            }
        }
        OptExpr::Object(_, entries) => {
            for (_, value) in entries {
                collect(value, used);
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
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
        OptExpr::Block {
            statements, result, ..
        } => {
            for statement in statements {
                collect(&statement.value, used);
            }
            collect(result, used);
        }
    }
}
