//! Conservative compile-time failability analysis over the optimization IR.
//!
//! [`can_fail`] answers "may evaluating this subtree raise an error?" — used to
//! decide when dropping an evaluation is observation-preserving. Statements are
//! evaluated eagerly, so an unread binding whose value may error must be kept
//! ([`RegisterPruner`](crate::second_pass_rules)); likewise an object-access
//! rewrite may drop a sibling value only when that value cannot fail
//! ([`ObjectAccessFold`](crate::rules)).
//!
//! It is a conservative over-approximation: a `true` answer never wrongly drops
//! an error (it keeps the work), and per-function failability is read from the
//! argument-independent [`ExprFunction::can_fail`](air_elt_expr_funcs::ExprFunction::can_fail)
//! (the type-aware pass can refine it later).

use air_elt_expr_funcs::FunctionRegistry;

use crate::model::opt_expr::OptExpr;

/// Whether evaluating `expr` may raise an error. Conservative: any fallible
/// function call anywhere in the subtree, or an interpolation (which can exceed
/// the string-size cap), or a `TypeAssert` (which exists to raise a preserved
/// `TypeMismatch`), makes the whole expression fallible.
pub(crate) fn can_fail(expr: &OptExpr, registry: &FunctionRegistry) -> bool {
    match expr {
        OptExpr::Const(..)
        | OptExpr::Register(..)
        | OptExpr::SourceField(..)
        | OptExpr::Fields(..) => false,
        OptExpr::Field(_, inner) => can_fail(inner, registry),
        OptExpr::Call { func, args, .. } => {
            registry.get_by_ref(*func).can_fail() || args.iter().any(|arg| can_fail(arg, registry))
        }
        OptExpr::If {
            condition,
            then_branch,
            else_branch,
            ..
        } => {
            can_fail(condition, registry)
                || can_fail(then_branch, registry)
                || can_fail(else_branch, registry)
        }
        OptExpr::MultiIf {
            branches, default, ..
        } => {
            branches.iter().any(|(condition, value)| {
                can_fail(condition, registry) || can_fail(value, registry)
            }) || can_fail(default, registry)
        }
        OptExpr::IfNull {
            value, alternative, ..
        } => can_fail(value, registry) || can_fail(alternative, registry),
        OptExpr::NullIf {
            value, sentinel, ..
        } => can_fail(value, registry) || can_fail(sentinel, registry),
        OptExpr::And { left, right, .. } | OptExpr::Or { left, right, .. } => {
            can_fail(left, registry) || can_fail(right, registry)
        }
        // Interpolation can raise `StringTooLarge` even with infallible segments.
        OptExpr::Interpolation(..) => true,
        OptExpr::Object(_, entries) => entries.iter().any(|(_, value)| can_fail(value, registry)),
        OptExpr::Switch {
            inputs,
            table,
            default,
            ..
        } => {
            inputs.iter().any(|input| can_fail(input, registry))
                || table.iter().any(|(_, value)| can_fail(value, registry))
                || can_fail(default, registry)
        }
        // A TypeAssert exists precisely to raise the preserved `TypeMismatch` on a
        // present, wrong-typed operand — so it can always fail.
        OptExpr::TypeAssert { .. } => true,
        // A block fails if any binding or its result can fail. Its bindings are
        // evaluated only when control reaches the block (it is a sub-expression),
        // mirroring the program-level eager statement semantics scoped to the
        // subtree.
        OptExpr::Block {
            statements, result, ..
        } => {
            statements
                .iter()
                .any(|statement| can_fail(&statement.value, registry))
                || can_fail(result, registry)
        }
    }
}
