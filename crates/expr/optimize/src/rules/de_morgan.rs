//! De Morgan factoring of negated boolean operands.
//!
//! `not(a) && not(b)` → `not(a || b)` and `not(a) || not(b)` → `not(a && b)`.
//! Only the **factoring** direction is applied — the one that merges the two
//! `not`s into a single outer `not` (−1 node). It is size-decreasing, so it lives
//! in the fixpoint and cannot oscillate (the count of `not` calls strictly
//! drops); the opposite, negation-distributing direction is never a rule.
//!
//! It composes with the not-involution round-trip ([`super::round_trip`]): once
//! the two `not`s collapse into one, an enclosing `not` forms `not(not(...))`,
//! which collapses to a `TypeAssert{Bool, Identity}` — `!(!a || !b)` →
//! `not(not(a && b))` → `TypeAssert{Bool, Identity}` over `a && b`.
//!
//! **Soundness (three-valued + short-circuit + errors).** Kleene `&&`/`||` and
//! `not` obey De Morgan, and the rewrite preserves operand order and the
//! short-circuit set: `not(a) && not(b)` skips its right operand exactly when
//! `a` is true, which is exactly when `not(a || b)` skips `b`; the same holds for
//! the dual. So evaluation order, the short-circuited operand, null propagation,
//! and which operand's error fires all match. The only observable difference is
//! the error variant on a non-bool operand (`not`'s `FuncError` vs `&&`/`||`'s
//! `ExpectedBool`) — the same benign divergence the `multiIf` → `if` collapse has.

use air_elt_expr_funcs::{FuncRef, FunctionRegistry};

use super::{Rewrite, Rule, RuleCx};
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;

pub(crate) struct DeMorgan {
    not: Option<FuncRef>,
}

impl DeMorgan {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            not: registry.get_ref("not", Some(1)).ok(),
        }
    }
}

impl Rule for DeMorgan {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let Some(not) = self.not else {
            return Rewrite::Same(node);
        };
        let counter = cx.node_counter;

        match node {
            OptExpr::And { id, left, right } => {
                match (peel_not(not, *left), peel_not(not, *right)) {
                    (Ok(a), Ok(b)) => Rewrite::Changed(wrap_not(
                        not,
                        counter,
                        OptExpr::Or {
                            id,
                            left: Box::new(a),
                            right: Box::new(b),
                        },
                    )),
                    (left, right) => Rewrite::Same(OptExpr::And {
                        id,
                        left: Box::new(restore_not(not, counter, left)),
                        right: Box::new(restore_not(not, counter, right)),
                    }),
                }
            }
            OptExpr::Or { id, left, right } => {
                match (peel_not(not, *left), peel_not(not, *right)) {
                    (Ok(a), Ok(b)) => Rewrite::Changed(wrap_not(
                        not,
                        counter,
                        OptExpr::And {
                            id,
                            left: Box::new(a),
                            right: Box::new(b),
                        },
                    )),
                    (left, right) => Rewrite::Same(OptExpr::Or {
                        id,
                        left: Box::new(restore_not(not, counter, left)),
                        right: Box::new(restore_not(not, counter, right)),
                    }),
                }
            }
            other => Rewrite::Same(other),
        }
    }
}

/// `Ok(operand)` if `expr` is `not(operand)`, else `Err(expr)` returned intact.
fn peel_not(not: FuncRef, expr: OptExpr) -> Result<OptExpr, OptExpr> {
    match expr {
        OptExpr::Call { func, mut args, .. } if func == not && args.len() == 1 => {
            Ok(args.swap_remove(0))
        }
        other => Err(other),
    }
}

/// Inverse of [`peel_not`]: rewrap a peeled operand, or pass through the original.
fn restore_not(not: FuncRef, counter: &NodeCounter, peeled: Result<OptExpr, OptExpr>) -> OptExpr {
    match peeled {
        Ok(operand) => wrap_not(not, counter, operand),
        Err(original) => original,
    }
}

fn wrap_not(not: FuncRef, counter: &NodeCounter, operand: OptExpr) -> OptExpr {
    OptExpr::Call {
        id: counter.fresh_id(),
        func: not,
        args: vec![operand],
    }
}
