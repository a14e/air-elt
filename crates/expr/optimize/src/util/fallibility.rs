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

use std::ops::ControlFlow;

use air_elt_expr_funcs::FunctionRegistry;

use crate::model::opt_expr::OptExpr;
use crate::util::visit::for_each_recursive;

/// Whether evaluating `expr` may raise an error. Conservative: any fallible
/// function call anywhere in the subtree, or an interpolation (which can exceed
/// the string-size cap), or a `TypeAssert` (which exists to raise a preserved
/// `TypeMismatch`), makes the whole expression fallible. A block's bindings are
/// evaluated whenever control reaches the block, so they count like any other
/// child.
pub(crate) fn can_fail(expr: &OptExpr, registry: &FunctionRegistry) -> bool {
    let scan = for_each_recursive(expr, &mut |node| {
        let node_fails = match node {
            // TODO (deferred): argument-independent per-function failability; the
            // type-aware pass could refine it (e.g. division by a non-zero const).
            OptExpr::Call { func, .. } => registry.get_by_ref(*func).can_fail(),
            // Interpolation can raise `StringTooLarge` even with infallible
            // segments.
            OptExpr::Interpolation(..) => true,
            // A TypeAssert exists precisely to raise the preserved `TypeMismatch`
            // on a present, wrong-typed operand — so it can always fail.
            OptExpr::TypeAssert { .. } => true,
            _ => false,
        };
        if node_fails {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    scan.is_break()
}
