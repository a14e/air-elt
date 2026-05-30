//! Nested-`Switch` collapse: a switch whose `default` is another switch over the
//! SAME key expressions merges into one dispatch table.
//!
//! `Switch{K, T1, default: Switch{K, T2, d2}}` → `Switch{K, T1 ∪ T2, d2}`, with
//! `T1`'s entries taking priority on a key collision (first-match order, as the
//! `multiIf` lowering preserves). The outer switch evaluates `K` once on a hit
//! and the inner re-evaluates it on a miss, so merging to a single evaluation is
//! sound only when `K` is pure — both evaluations must agree. The keys are
//! compared structurally, so the two switches must read exactly the same
//! expressions.

use ahash::AHashSet;
use air_elt_types::Key;

use super::switch_build::is_pure;
use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct SwitchCollapse;

impl Rule for SwitchCollapse {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        let OptExpr::Switch {
            inputs,
            table,
            default,
        } = node
        else {
            return Rewrite::Same(node);
        };

        let default_is_same_key_switch = match &*default {
            OptExpr::Switch {
                inputs: inner_inputs,
                ..
            } => *inner_inputs == inputs,
            _ => false,
        };
        let collapsible =
            default_is_same_key_switch && inputs.iter().all(|expr| is_pure(expr, cx.registry));
        if !collapsible {
            return Rewrite::Same(OptExpr::Switch {
                inputs,
                table,
                default,
            });
        }

        match *default {
            OptExpr::Switch {
                table: inner_table,
                default: inner_default,
                ..
            } => {
                let mut merged = table;
                let mut seen: AHashSet<Key> = merged.iter().map(|(key, _)| key.clone()).collect();
                for (key, value) in inner_table {
                    // Outer entries win; only fresh keys carry over from the inner.
                    if seen.insert(key.clone()) {
                        merged.push((key, value));
                    }
                }
                Rewrite::Changed(OptExpr::Switch {
                    inputs,
                    table: merged,
                    default: inner_default,
                })
            }
            // `collapsible` already proved the default is a switch.
            other => Rewrite::Same(OptExpr::Switch {
                inputs,
                table,
                default: Box::new(other),
            }),
        }
    }
}
