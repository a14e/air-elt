//! Post-fixpoint collapse of a small `multiIf` back into `if` / nested `if`.
//!
//! This is the inverse of [`flatten_conditionals`](super::flatten_conditionals),
//! which canonicalizes `if` chains into one `multiIf` *inside* the fixpoint to
//! expose dead-branch pruning and the switch-lowering shape. Once the fixpoint
//! has settled — dead branches pruned, conforming `multiIf`s lowered to a
//! [`Switch`](OptExpr::Switch) — a `multiIf` that stayed small and un-lowered is
//! better expressed as `if`: that is the form the runtime executes and the shape
//! future `if`-specific peepholes match.
//!
//! **Why this is a one-shot finalizer, not a fixpoint rule.** Running it in the
//! fixpoint would oscillate against `flatten_conditionals`: the collapse produces
//! a nested `if` whose else branch is itself an `if`, which `flatten_conditionals`
//! would immediately re-merge into a `multiIf`. As a single post-fixpoint sweep
//! (driven by [`RewriteDriver`](super::RewriteDriver)'s `finalize`) there is
//! no feedback, so termination is structural — each `multiIf` is rewritten once
//! and yields only `if` nodes, which this rule never re-matches.
//!
//! **Scope.** Only `multiIf`s with at most [`MAX_COLLAPSE_BRANCHES`] branches are
//! collapsed; a larger one is either a `Switch` already or genuinely wants the
//! flat form. There is no condition-shape guard: a `multiIf` this small is never
//! a switch candidate (switch lowering needs `> 5` branches), so an or-keyed
//! condition is safe to collapse just like any other — `if` keeps the `Or`
//! intact and evaluates it identically.
//!
//! **Soundness.** `if` and `multiIf` share branch semantics exactly (see the
//! evaluator: a false/null condition falls through, a non-bool condition errors),
//! so the rewrite is meaning-preserving. The only observable difference is the
//! error-context label (`"multiIf"` → `"if"`) on a non-bool condition.

use super::{Rewrite, Rule, RuleCx};
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;

/// Largest branch count a `multiIf` may have to still collapse. A single-branch
/// `multiIf` becomes one `if`; a two-branch one becomes a nested `if`. Above
/// this the `multiIf`/`Switch` form is kept.
const MAX_COLLAPSE_BRANCHES: usize = 2;

pub(crate) struct MultiIfCollapse;

impl Rule for MultiIfCollapse {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::MultiIf {
            id,
            branches,
            default,
        } = node
        else {
            return Rewrite::Same(node);
        };

        if branches.len() > MAX_COLLAPSE_BRANCHES {
            return Rewrite::Same(OptExpr::MultiIf {
                id,
                branches,
                default,
            });
        }

        Rewrite::Changed(fold_into_ifs(branches, *default, cx.node_counter))
    }
}

/// Fold `(condition, value)` branches right-to-left into nested `if`s, so the
/// first branch is the outermost test and `default` is the innermost else.
fn fold_into_ifs(
    branches: Vec<(OptExpr, OptExpr)>,
    default: OptExpr,
    node_counter: &NodeCounter,
) -> OptExpr {
    let mut else_branch = default;
    for (condition, value) in branches.into_iter().rev() {
        else_branch = OptExpr::If {
            id: node_counter.fresh_id(),
            condition: Box::new(condition),
            then_branch: Box::new(value),
            else_branch: Box::new(else_branch),
        };
    }
    else_branch
}
