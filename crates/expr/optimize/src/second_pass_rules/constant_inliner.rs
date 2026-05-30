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
use crate::model::opt_expr::OptExpr;
use crate::model::opt_program::OptProgram;

pub(crate) struct ConstantInliner;

impl ProgramPass for ConstantInliner {
    fn run(&self, program: &mut OptProgram, _registry: &FunctionRegistry) {
        let mut constants: AHashMap<u16, Value> = AHashMap::new();
        let statements = std::mem::take(&mut program.statements);
        let mut kept = Vec::with_capacity(statements.len());

        for mut statement in statements {
            substitute(&mut statement.value, &constants);
            if let OptExpr::Const(value) = &statement.value {
                constants.insert(statement.register, value.clone());
            } else {
                kept.push(statement);
            }
        }

        substitute(&mut program.result, &constants);
        program.statements = kept;
    }
}

/// Replace `Register(r)` with `Const(v)` wherever a known constant applies.
fn substitute(expr: &mut OptExpr, constants: &AHashMap<u16, Value>) {
    match expr {
        OptExpr::Register(register) => {
            if let Some(value) = constants.get(register) {
                *expr = OptExpr::Const(value.clone());
            }
        }
        OptExpr::Const(_) | OptExpr::SourceField(_) | OptExpr::Fields(_) => {}
        OptExpr::Field(inner) => substitute(inner, constants),
        OptExpr::Call { args, .. } => {
            for arg in args {
                substitute(arg, constants);
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
        } => {
            substitute(condition, constants);
            substitute(then_branch, constants);
            substitute(else_branch, constants);
        }
        OptExpr::MultiIf { branches, default } => {
            for (condition, value) in branches {
                substitute(condition, constants);
                substitute(value, constants);
            }
            substitute(default, constants);
        }
        OptExpr::IfNull { value, alternative } => {
            substitute(value, constants);
            substitute(alternative, constants);
        }
        OptExpr::NullIf { value, sentinel } => {
            substitute(value, constants);
            substitute(sentinel, constants);
        }
        OptExpr::And { left, right } | OptExpr::Or { left, right } => {
            substitute(left, constants);
            substitute(right, constants);
        }
        OptExpr::Interpolation(segments) => {
            for segment in segments {
                substitute(segment, constants);
            }
        }
        OptExpr::Object(entries) => {
            for (_, value) in entries {
                substitute(value, constants);
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
        } => {
            for input in inputs {
                substitute(input, constants);
            }
            for (_, value) in table {
                substitute(value, constants);
            }
            substitute(default, constants);
        }
        OptExpr::TypeAssert { inner, .. } => substitute(inner, constants),
    }
}
