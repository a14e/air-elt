//! Constant inlining: a statement whose value folded to a constant is
//! substituted at every use, and its binding dropped.
//!
//! Statements are processed in source order, so a constant bound earlier is
//! already known when a later statement (or the result) is rewritten. This is a
//! size-non-increasing program pass: it only replaces register reads with
//! constants and removes bindings.
//!
//! [`OptExpr::Block`] bindings inline the same way, scoped naturally by SSA:
//! registers are program-wide unique, so a constant bound inside a block can
//! simply join the shared map — its register is only ever read within that
//! block's subtree. A constant block binding is dropped from the block (a
//! constant cannot fail, so no error is lost; an emptied block is collapsed by
//! the register pruner that runs next).

use std::ops::ControlFlow;

use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_types::Value;

use super::engine::ProgramPass;
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::{OptProgram, OptStatement};
use crate::util::visit::for_each_child_mut;

pub(crate) struct ConstantInliner;

impl ProgramPass for ConstantInliner {
    fn run(
        &self,
        program: &mut OptProgram,
        _registry: &FunctionRegistry,
        node_counter: &NodeCounter,
    ) {
        let mut constants: AHashMap<u16, Value> = AHashMap::new();
        program
            .statements
            .retain_mut(|statement| inline_statement(statement, &mut constants, node_counter));
        substitute(&mut program.result, &mut constants, node_counter);
    }
}

/// Substitute known constants into one binding; if the binding itself is (or
/// became) a constant, record it and report the statement as droppable.
fn inline_statement(
    statement: &mut OptStatement,
    constants: &mut AHashMap<u16, Value>,
    node_counter: &NodeCounter,
) -> bool {
    substitute(&mut statement.value, constants, node_counter);
    if let OptExpr::Const(_, value) = &mut statement.value {
        // The statement is dropped right after, so move the value out instead
        // of cloning it (a Text/Json constant can be large).
        let moved = std::mem::replace(value, Value::Null);
        constants.insert(statement.register, moved);
        false
    } else {
        true
    }
}

/// Replace `Register(r)` with `Const(v)` wherever a known constant applies. The
/// map is mutable because a block contributes its own constant bindings while
/// it is walked (mirroring the program-level statement loop).
fn substitute(
    expr: &mut OptExpr,
    constants: &mut AHashMap<u16, Value>,
    node_counter: &NodeCounter,
) {
    match expr {
        OptExpr::Register(_, register) => {
            if let Some(value) = constants.get(register) {
                *expr = OptExpr::Const(node_counter.fresh_id(), value.clone());
            }
        }
        OptExpr::Block {
            statements, result, ..
        } => {
            // Mirror the program-level loop: dropping a constant binding is
            // safe (a constant cannot fail) and SSA registers keep the shared
            // map scope-correct.
            statements.retain_mut(|statement| inline_statement(statement, constants, node_counter));
            substitute(result, constants, node_counter);
        }
        other => {
            let _: ControlFlow<()> = for_each_child_mut(other, |child| {
                substitute(child, constants, node_counter);
                ControlFlow::Continue(())
            });
        }
    }
}
