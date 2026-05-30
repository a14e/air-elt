//! OR-of-equals → set-membership `Switch`.
//!
//! A long disjunction of equality tests over the same key(s) —
//! `k == c1 || k == c2 || … || k == cN` (or composite `(k1==a && k2==b) || …`) —
//! is a set-membership predicate. It lowers directly to an O(1)
//! [`OptExpr::Switch`] whose every listed key maps to `true` and whose `default`
//! is `false`.
//!
//! **Soundness** rests on comparisons being TOTAL `Bool` (Phase 2h): each
//! `equals(K, const)` is non-null `Bool`, so the `||` chain is a total `Bool` and
//! the generic three-valued `a || b` hazard (`null || false` is `null`, not
//! `false`) never arises. A null `K` makes every `equals` `false`; the switch's
//! `Key::from_value(Null)` misses the table and falls to `default = false` — the
//! same result. `K` is evaluated once instead of per clause, so it must be pure;
//! any error in `K` surfaces identically (it is the leftmost thing evaluated
//! either way). Constants, key arity, and purity are gated through the shared
//! [`switch_build`](super::switch_build), exactly as [`switch_lower`](super::switch_lower).
//!
//! This is a **finalize** rule: running it inside the fixpoint would lower a
//! `multiIf` branch condition that is itself a long OR before
//! [`switch_lower`](super::switch_lower) could turn the whole `multiIf` into a
//! value dispatch. After the fixpoint, switchable `multiIf`s are already
//! `Switch`es, so only genuinely standalone membership predicates remain here.

use ahash::AHashSet;
use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::{Key, Value};

use super::switch_build::{KeyExprs, SwitchEntries, clause_to_key, is_pure, parse_condition};
use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

/// Minimum distinct membership entries for the table to pay off (strictly `> 5`).
const MIN_MEMBERSHIP_ENTRIES: usize = 6;

pub(crate) struct OrMembership {
    equals: Option<FuncRef>,
}

impl OrMembership {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            equals: registry.get_ref("equals", Some(2)).ok(),
        }
    }
}

impl Rule for OrMembership {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        if !matches!(node, OptExpr::Or { .. }) {
            return Rewrite::Same(node);
        }
        let Some(equals) = self.equals else {
            return Rewrite::Same(node);
        };

        match try_lower(&node, equals, cx.registry) {
            Some((inputs, table)) => Rewrite::Changed(OptExpr::Switch {
                inputs,
                table,
                default: Box::new(OptExpr::Const(Value::Bool(false))),
            }),
            None => Rewrite::Same(node),
        }
    }
}

fn try_lower(
    condition: &OptExpr,
    equals: FuncRef,
    registry: &FunctionRegistry,
) -> Option<(KeyExprs, SwitchEntries)> {
    let clauses = parse_condition(condition, equals)?;
    if clauses.len() < MIN_MEMBERSHIP_ENTRIES {
        // A short chain is cheaper left as `||`; only a long membership test pays
        // for the table.
        return None;
    }

    let mut key_exprs: Option<KeyExprs> = None;
    let mut table: SwitchEntries = Vec::new();
    let mut seen: AHashSet<Key> = AHashSet::new();
    for clause in clauses {
        let key = clause_to_key(clause, &mut key_exprs)?;
        // First-match wins: a repeated key keeps its first (identical) `true`.
        if !seen.insert(key.clone()) {
            continue;
        }
        table.push((key, OptExpr::Const(Value::Bool(true))));
    }

    let key_exprs = key_exprs?;
    if table.len() < MIN_MEMBERSHIP_ENTRIES {
        return None;
    }
    // The switch evaluates each key expression exactly once.
    if !key_exprs.iter().all(|expr| is_pure(expr, registry)) {
        return None;
    }
    Some((key_exprs, table))
}
