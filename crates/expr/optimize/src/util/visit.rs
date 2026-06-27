//! Generic child visitors for [`OptExpr`] — the single exhaustive child-slot
//! enumeration shared by passes that walk the tree (constant inlining, block
//! pruning, purity/fallibility scans, field hoisting). A pass matches the
//! variants it treats specially and delegates every other variant here, so
//! adding a new `OptExpr` variant updates the two matches in this file instead
//! of one per pass.
//!
//! Both visitors use [`ControlFlow`]: the callback returns
//! `ControlFlow::Continue(())` to keep walking or `ControlFlow::Break(value)`
//! to stop early; the visitor propagates the break (with its payload) to the
//! caller. Idioms: any-scan — break on a hit and check `.is_break()`;
//! all-predicate — break on a violation and check `.is_continue()`;
//! infallible walk — always continue and discard the result.

use std::ops::ControlFlow;

use crate::model::opt_expr::OptExpr;

/// Invoke `visit` on `expr` and every node (pre-order), stopping at the
/// first break. The common whole-subtree scan; use [`for_each_child`] directly
/// when the pass needs its own recursion order or per-variant handling.
pub(crate) fn for_each_recursive<B, F>(expr: &OptExpr, visit: &mut F) -> ControlFlow<B>
where
    F: FnMut(&OptExpr) -> ControlFlow<B>,
{
    visit(expr)?;
    for_each_child(expr, |child| for_each_recursive(child, visit))
}

/// Mutable twin of [`for_each_recursive`]: a node replaced by the callback is
/// walked in its new shape (children are enumerated after the visit).
pub(crate) fn for_each_recursive_mut<B, F>(expr: &mut OptExpr, visit: &mut F) -> ControlFlow<B>
where
    F: FnMut(&mut OptExpr) -> ControlFlow<B>,
{
    visit(expr)?;
    for_each_child_mut(expr, |child| for_each_recursive_mut(child, visit))
}

/// Invoke `visit` on every direct child expression of `expr`, stopping at the
/// first break. Does not recurse — the caller's callback drives the recursion,
/// so it stays in control of pre/post order and of variants it handles itself.
pub(crate) fn for_each_child<B, F>(expr: &OptExpr, mut visit: F) -> ControlFlow<B>
where
    F: FnMut(&OptExpr) -> ControlFlow<B>,
{
    match expr {
        OptExpr::Const(..)
        | OptExpr::Register(..)
        | OptExpr::SourceField(..)
        | OptExpr::Fields(..) => {}
        OptExpr::Field(_, inner) => visit(inner)?,
        OptExpr::Call { args, .. } => {
            for arg in args {
                visit(arg)?;
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit(condition)?;
            visit(then_branch)?;
            visit(else_branch)?;
        }
        OptExpr::MultiIf {
            branches, default, ..
        } => {
            for (condition, value) in branches {
                visit(condition)?;
                visit(value)?;
            }
            visit(default)?;
        }
        OptExpr::IfNull {
            value, alternative, ..
        } => {
            visit(value)?;
            visit(alternative)?;
        }
        OptExpr::NullIf {
            value, sentinel, ..
        } => {
            visit(value)?;
            visit(sentinel)?;
        }
        OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
            visit(left)?;
            visit(right)?;
        }
        OptExpr::Interpolation(_, segments) => {
            for segment in segments {
                visit(segment)?;
            }
        }
        OptExpr::Object(_, entries) => {
            for (_, value) in entries {
                visit(value)?;
            }
        }
        OptExpr::Array(_, elements) => {
            for element in elements {
                visit(element)?;
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            for input in inputs {
                visit(input)?;
            }
            for (_, value) in table {
                visit(value)?;
            }
            visit(default)?;
        }
        OptExpr::TypeAssert { inner, .. } => visit(inner)?,
        OptExpr::Block {
            statements, result, ..
        } => {
            for statement in statements {
                visit(&statement.value)?;
            }
            visit(result)?;
        }
    }
    ControlFlow::Continue(())
}

/// Mutable twin of [`for_each_child`]: same child-slot enumeration, same
/// early-exit contract, `&mut` access to each child.
pub(crate) fn for_each_child_mut<B, F>(expr: &mut OptExpr, mut visit: F) -> ControlFlow<B>
where
    F: FnMut(&mut OptExpr) -> ControlFlow<B>,
{
    match expr {
        OptExpr::Const(..)
        | OptExpr::Register(..)
        | OptExpr::SourceField(..)
        | OptExpr::Fields(..) => {}
        OptExpr::Field(_, inner) => visit(inner)?,
        OptExpr::Call { args, .. } => {
            for arg in args {
                visit(arg)?;
            }
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            visit(condition)?;
            visit(then_branch)?;
            visit(else_branch)?;
        }
        OptExpr::MultiIf {
            branches, default, ..
        } => {
            for (condition, value) in branches {
                visit(condition)?;
                visit(value)?;
            }
            visit(default)?;
        }
        OptExpr::IfNull {
            value, alternative, ..
        } => {
            visit(value)?;
            visit(alternative)?;
        }
        OptExpr::NullIf {
            value, sentinel, ..
        } => {
            visit(value)?;
            visit(sentinel)?;
        }
        OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
            visit(left)?;
            visit(right)?;
        }
        OptExpr::Interpolation(_, segments) => {
            for segment in segments {
                visit(segment)?;
            }
        }
        OptExpr::Object(_, entries) => {
            for (_, value) in entries {
                visit(value)?;
            }
        }
        OptExpr::Array(_, elements) => {
            for element in elements {
                visit(element)?;
            }
        }
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            for input in inputs {
                visit(input)?;
            }
            for (_, value) in table {
                visit(value)?;
            }
            visit(default)?;
        }
        OptExpr::TypeAssert { inner, .. } => visit(inner)?,
        OptExpr::Block {
            statements, result, ..
        } => {
            for statement in statements {
                visit(&mut statement.value)?;
            }
            visit(result)?;
        }
    }
    ControlFlow::Continue(())
}
