//! Structural scan for [`OptExpr::Block`] occurrences.
//!
//! Blocks must never be cloned into several surviving positions: their register
//! writes would alias (see the invariant on [`OptExpr::Block`]). A rule that
//! duplicates a subtree — today only switch lowering, which copies one branch
//! value into a table entry per matching key — consults [`contains_block`] and
//! bails out when the subtree carries a block anywhere inside it.

use std::ops::ControlFlow;

use crate::model::opt_expr::OptExpr;
use crate::util::visit::for_each_recursive;

/// Whether `expr` contains an [`OptExpr::Block`] anywhere in its subtree
/// (including `expr` itself).
pub(crate) fn contains_block(expr: &OptExpr) -> bool {
    let scan = for_each_recursive(expr, &mut |node| {
        if matches!(node, OptExpr::Block { .. }) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    scan.is_break()
}
