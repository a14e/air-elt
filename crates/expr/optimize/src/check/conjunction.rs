//! Conjunction infeasibility: a `&&` chain that asserts the same operand is two
//! incompatible type classes can never be true for any non-null row — every
//! non-null value raises a type error — so it is rejected at compile time.
//!
//! Only a [`TypeAssert`](OptExpr::TypeAssert) carries an explicit type
//! requirement the untyped optimizer can read, so this is the one infeasibility
//! detectable in Phase 2. Comparison operators return `false` (they do not raise)
//! on a type mismatch, so a `x > 1 && x > ""` shape is merely always-`false`, not
//! infeasible — proving that needs the typed Phase-3 pass. Like the other
//! categorical checks this fires in every position: a conjunction that always
//! raises is a definite bug regardless of which branch reaches it.

use ahash::AHashMap;

use super::engine::{Check, CheckCx};
use crate::error::OptimizeError;
use crate::model::opt_expr::{FrozenOperand, OptExpr};
use crate::model::program::TypeClass;

pub(crate) struct ConjunctionInfeasibility;

impl Check for ConjunctionInfeasibility {
    fn check(&self, node: &OptExpr, _eager: bool, _cx: &CheckCx) -> Result<(), OptimizeError> {
        let OptExpr::And { .. } = node else {
            return Ok(());
        };

        let mut conjuncts = Vec::new();
        flatten_and(node, &mut conjuncts);

        // The type class each conjunct asserts on a frozen operand, keyed by
        // operand for O(1) conflict lookup. A later assert on the same operand
        // demanding a different class is the contradiction.
        let mut asserted: AHashMap<FrozenOperand, TypeClass> = AHashMap::new();
        for conjunct in conjuncts {
            let OptExpr::TypeAssert { inner, expect, .. } = conjunct else {
                continue;
            };
            let Some(key) = inner.frozen_operand() else {
                continue;
            };
            if let Some(other) = asserted.get(&key) {
                if *other != *expect {
                    return Err(OptimizeError::InfeasibleConjunction {
                        first: other.describe(),
                        second: expect.describe(),
                    });
                }
            } else {
                asserted.insert(key, *expect);
            }
        }
        Ok(())
    }
}

/// Collect the operands of a `&&` tree (the conjuncts), descending through nested
/// `&&` nodes and treating everything else as a leaf conjunct.
fn flatten_and<'a>(node: &'a OptExpr, out: &mut Vec<&'a OptExpr>) {
    match node {
        OptExpr::And { left, right, .. } => {
            flatten_and(left, out);
            flatten_and(right, out);
        }
        other => out.push(other),
    }
}
