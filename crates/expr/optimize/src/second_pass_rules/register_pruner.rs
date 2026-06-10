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
//!
//! The same rule applies inside [`OptExpr::Block`]s: a block's bindings run
//! exactly when the block is reached, so an unread, infallible block binding is
//! dropped the same way (registers are SSA — program-wide unique — so the use
//! scan needs no scoping). A block whose statements all pruned away collapses
//! to its bare result expression.

use std::ops::ControlFlow;

use ahash::AHashSet;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_types::Value;

use super::engine::ProgramPass;
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::OptProgram;
use crate::util::fallibility::can_fail;
use crate::util::visit::{for_each_child_mut, for_each_recursive};

pub(crate) struct RegisterPruner;

impl ProgramPass for RegisterPruner {
    fn run(
        &self,
        program: &mut OptProgram,
        registry: &FunctionRegistry,
        node_counter: &NodeCounter,
    ) {
        loop {
            let mut used: AHashSet<u16> = AHashSet::new();
            collect(&program.result, &mut used);
            for statement in &program.statements {
                collect(&statement.value, &mut used);
            }

            let mut changed = false;
            let before = program.statements.len();
            program.statements.retain(|statement| {
                used.contains(&statement.register) || can_fail(&statement.value, registry)
            });
            changed |= program.statements.len() != before;

            for statement in &mut program.statements {
                changed |= prune_blocks(&mut statement.value, &used, registry, node_counter);
            }
            changed |= prune_blocks(&mut program.result, &used, registry, node_counter);

            if !changed {
                break;
            }
        }
    }
}

/// Prune unread, infallible bindings inside every [`OptExpr::Block`] in the
/// subtree, collapsing a block left with zero statements to its bare result.
/// Returns whether anything changed (driving the caller's fixpoint).
fn prune_blocks(
    expr: &mut OptExpr,
    used: &AHashSet<u16>,
    registry: &FunctionRegistry,
    node_counter: &NodeCounter,
) -> bool {
    let mut changed = false;
    let _: ControlFlow<()> = for_each_child_mut(expr, |child| {
        changed |= prune_blocks(child, used, registry, node_counter);
        ControlFlow::Continue(())
    });
    if let OptExpr::Block {
        statements, result, ..
    } = expr
    {
        let before = statements.len();
        statements.retain(|statement| {
            used.contains(&statement.register) || can_fail(&statement.value, registry)
        });
        changed |= statements.len() != before;

        if statements.is_empty() {
            // `Block { [], result }` ≡ `result` — swap the result out (the
            // placeholder constant is discarded with the block shell).
            let placeholder = OptExpr::Const(node_counter.fresh_id(), Value::Null);
            let collapsed = std::mem::replace(result.as_mut(), placeholder);
            *expr = collapsed;
            changed = true;
        }
    }
    changed
}

/// Record every register read in the subtree into `used`.
fn collect(expr: &OptExpr, used: &mut AHashSet<u16>) {
    let _: ControlFlow<()> = for_each_recursive(expr, &mut |node| {
        if let OptExpr::Register(_, register) = node {
            used.insert(*register);
        }
        ControlFlow::Continue(())
    });
}
