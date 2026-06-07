//! Constant inlining: a statement whose value folded to a constant is
//! substituted at every use, and its binding dropped.
//!
//! Statements are processed in source order, so a constant bound earlier is
//! already known when a later statement (or the result) is rewritten. This is a
//! size-non-increasing program pass: it only replaces register reads with
//! constants and removes bindings.

use ahash::AHashMap;
use air_elt_expr_funcs::FunctionRegistry;
use air_elt_types::Value;

use super::engine::ProgramPass;
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::OptProgram;

pub(crate) struct ConstantInliner;

impl ProgramPass for ConstantInliner {
    fn run(
        &self,
        program: &mut OptProgram,
        _registry: &FunctionRegistry,
        node_counter: &NodeCounter,
    ) {
        let mut constants: AHashMap<u16, Value> = AHashMap::new();
        let statements = std::mem::take(&mut program.statements);
        let mut kept = Vec::with_capacity(statements.len());

        for mut statement in statements {
            substitute(&mut statement.value, &constants, node_counter);
            if let OptExpr::Const(_, value) = &statement.value {
                constants.insert(statement.register, value.clone());
            } else {
                kept.push(statement);
            }
        }

        substitute(&mut program.result, &constants, node_counter);
        program.statements = kept;
    }
}

/// Replace `Register(r)` with `Const(v)` wherever a known constant applies.
fn substitute(expr: &mut OptExpr, constants: &AHashMap<u16, Value>, node_counter: &NodeCounter) {
    match expr {
        OptExpr::Register(_, register) => {
            if let Some(value) = constants.get(register) {
                *expr = OptExpr::Const(node_counter.fresh_id(), value.clone());
            }
        }
        OptExpr::Const(..) | OptExpr::SourceField(..) | OptExpr::Fields(..) => {}
        OptExpr::Field(_, inner) => substitute(inner, constants, node_counter),
        OptExpr::Call { args, .. } => {
            for arg in args {
                substitute(arg, constants, node_counter);
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            substitute(condition, constants, node_counter);
            substitute(then_branch, constants, node_counter);
            substitute(else_branch, constants, node_counter);
        }
        OptExpr::MultiIf {
            branches, default, ..
        } => {
            for (condition, value) in branches {
                substitute(condition, constants, node_counter);
                substitute(value, constants, node_counter);
            }
            substitute(default, constants, node_counter);
        }
        OptExpr::IfNull {
            value, alternative, ..
        } => {
            substitute(value, constants, node_counter);
            substitute(alternative, constants, node_counter);
        }
        OptExpr::NullIf {
            value, sentinel, ..
        } => {
            substitute(value, constants, node_counter);
            substitute(sentinel, constants, node_counter);
        }
        OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
            substitute(left, constants, node_counter);
            substitute(right, constants, node_counter);
        }
        OptExpr::Interpolation(_, segments) => {
            for segment in segments {
                substitute(segment, constants, node_counter);
            }
        }
        OptExpr::Object(_, entries) => {
            for (_, value) in entries {
                substitute(value, constants, node_counter);
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            for input in inputs {
                substitute(input, constants, node_counter);
            }
            for (_, value) in table {
                substitute(value, constants, node_counter);
            }
            substitute(default, constants, node_counter);
        }
        OptExpr::TypeAssert { inner, .. } => substitute(inner, constants, node_counter),
        OptExpr::Block {
            statements, result, ..
        } => {
            for statement in statements {
                substitute(&mut statement.value, constants, node_counter);
            }
            substitute(result, constants, node_counter);
        }
    }
}
