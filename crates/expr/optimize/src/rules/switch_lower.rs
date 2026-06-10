//! Switch lowering: a large `multiIf` of equality tests becomes an O(1)
//! constant-key dispatch ([`OptExpr::Switch`]).
//!
//! Fires only when EVERY branch condition is a disjunction (`||`) of clauses,
//! each clause a conjunction (`&&`) of `equals(K, const)` tests over the SAME
//! one or two key expressions `K`. Guards keep the rewrite sound:
//! * **threshold > 5 branches** — below that a linear `multiIf` beats the
//!   hashmap's build/lookup overhead;
//! * **allow-listed constants** (`Int*`/`UInt*`/`BigInt`/`Text`/`Bool`/`Uuid`,
//!   non-null) — excludes `Float`/`Decimal`/… so key hashing is well-defined
//!   (no NaN, no float-equality surprises);
//! * **pure key expressions** — the switch evaluates `K` once, a `multiIf`
//!   re-evaluates the condition per branch, so `K` must be deterministic;
//! * **all-or-nothing** — any non-conforming branch leaves the `multiIf` intact;
//! * **no block values** — a branch value containing an [`OptExpr::Block`] is
//!   never lowered: the rewrite clones one value into several table entries, and
//!   cloning a block would alias its register writes.
//!
//! An `or` of clauses expands to several table entries pointing at one branch;
//! duplicate keys keep the first (preserving `multiIf` first-match order). The
//! clause parser, the constant allow-list, and the [`Key`] builder are shared
//! with [`or_membership`](super::or_membership) via
//! [`switch_build`](super::switch_build); the purity walk is
//! [`type_utils::is_pure`](crate::util::type_utils::is_pure).

use ahash::AHashSet;
use air_elt_expr_funcs::{FuncRef, FunctionRegistry};
use air_elt_types::Key;

use super::switch_build::{KeyExprs, SwitchEntries, clause_to_key, parse_condition};
use super::{Rewrite, Rule, RuleCx};
use crate::model::node_id::NodeCounter;
use crate::model::opt_expr::OptExpr;
use crate::util::block_scan::contains_block;
use crate::util::type_utils::is_pure;

/// Minimum branch count for the lookup table to pay off (strictly `> 5`).
const MIN_SWITCH_BRANCHES: usize = 6;

pub(crate) struct SwitchLower {
    equals: Option<FuncRef>,
}

impl SwitchLower {
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        Self {
            equals: registry.get_ref("equals", Some(2)).ok(),
        }
    }
}

impl Rule for SwitchLower {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::MultiIf {
            id,
            branches,
            default,
        } = node
        else {
            return Rewrite::Same(node);
        };
        let Some(equals) = self.equals else {
            return Rewrite::Same(OptExpr::MultiIf {
                id,
                branches,
                default,
            });
        };
        if branches.len() < MIN_SWITCH_BRANCHES {
            return Rewrite::Same(OptExpr::MultiIf {
                id,
                branches,
                default,
            });
        }

        match try_lower(&branches, equals, cx.registry, cx.node_counter) {
            Some((inputs, table)) => Rewrite::Changed(OptExpr::Switch {
                id: cx.node_counter.fresh_id(),
                inputs,
                table,
                default,
            }),
            None => Rewrite::Same(OptExpr::MultiIf {
                id,
                branches,
                default,
            }),
        }
    }
}

/// Attempt to read the branches as a constant-key dispatch. Returns the key
/// expressions and the `(Key, branch)` table on success.
fn try_lower(
    branches: &[(OptExpr, OptExpr)],
    equals: FuncRef,
    registry: &FunctionRegistry,
    node_counter: &NodeCounter,
) -> Option<(KeyExprs, SwitchEntries)> {
    let mut key_exprs: Option<KeyExprs> = None;
    let mut table: SwitchEntries = Vec::new();
    // `Key` is hashable with a cross-numeric-consistent `Eq`/`Hash`, so dedup is
    // an O(1) set membership rather than a linear table scan — the table can hold
    // thousands of entries for a generated dispatch.
    let mut seen: AHashSet<Key> = AHashSet::new();

    for (condition, value) in branches {
        // A branch value is CLONED into one table entry per key it matches. A
        // `Block` must never be duplicated — its register writes would alias
        // (`reassign_ids` restamps node ids, not registers) — so any branch
        // value carrying a block keeps the `multiIf` intact.
        // TODO (deferred): lift this bail by restamping block registers on
        // clone — needs the register allocator plumbed into the rules.
        if contains_block(value) {
            return None;
        }
        let clauses = parse_condition(condition, equals)?;
        if clauses.is_empty() {
            return None;
        }
        for clause in clauses {
            let key = clause_to_key(clause, &mut key_exprs)?;
            // First-match wins: a key already seen keeps its branch.
            if !seen.insert(key.clone()) {
                continue;
            }
            // One source branch value is cloned into a table entry per key it
            // matches; re-id each clone so the surviving copies keep distinct
            // node ids (the source branches are dropped, so this stays unique).
            let mut branch_value = value.clone();
            branch_value.reassign_ids(node_counter);
            table.push((key, branch_value));
        }
    }

    let key_exprs = key_exprs?;
    if table.is_empty() {
        return None;
    }
    // The switch evaluates each key expression exactly once.
    if !key_exprs.iter().all(|expr| is_pure(expr, registry)) {
        return None;
    }
    // A surviving key expression is one clone of a clause operand whose other
    // copies are dropped, so a block inside it cannot alias today — but only
    // because block SSA registers defeat the cross-clause structural-equality
    // check above. Keep the no-clone invariant local instead of emergent.
    if key_exprs.iter().any(contains_block) {
        return None;
    }
    Some((key_exprs, table))
}
