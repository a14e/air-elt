//! Dead-branch elimination and constant short-circuiting.
//!
//! When a conditional's controlling value is constant, the branch that can
//! never run is dropped. The boolean operators use the same three-valued
//! (`true`/`false`/`null`) semantics as the heap evaluator, so the optimizer
//! and the runtime always agree. These rules run BEFORE the type-check pass,
//! so they cannot assume boolean operands: when a fold makes the right operand
//! the whole result (`false || x`, `true && x`), the heap evaluator's "right
//! operand must be Bool/Null" check is preserved as a `TypeAssert{Bool}`
//! (stripped later by the typed engine when the operand is provably Bool). A
//! `Switch` whose key inputs are all constant folds to the matched branch (or
//! the default on a miss) — its key is built exactly as the evaluator builds
//! it, so the dispatch is resolved at compile time.

use air_elt_types::{Key, Value};

use super::{Rewrite, Rule, RuleCx};
use crate::model::node_id::NodeId;
use crate::model::opt_expr::{AssertYield, OptExpr};
use crate::model::program::TypeClass;

pub(crate) struct BranchPrune;

impl Rule for BranchPrune {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite {
        match node {
            OptExpr::If {
                id,
                condition,
                then_branch,
                else_branch,
            } => self.prune_if(id, condition, then_branch, else_branch),
            OptExpr::MultiIf {
                id,
                branches,
                default,
            } => self.prune_multi_if(id, branches, default),
            OptExpr::IfNull {
                id,
                value,
                alternative,
            } => self.prune_if_null(id, value, alternative),
            OptExpr::NullIf {
                id,
                value,
                sentinel,
            } => self.prune_null_if(id, value, sentinel, cx),
            OptExpr::And { id, left, right } => self.prune_and(id, left, right, cx),
            OptExpr::Or { id, left, right } => self.prune_or(id, left, right, cx),
            OptExpr::Switch {
                id,
                inputs,
                table,
                default,
            } => self.prune_switch(id, inputs, table, default),
            other => Rewrite::Same(other),
        }
    }
}

impl BranchPrune {
    fn prune_if(
        &self,
        id: NodeId,
        condition: Box<OptExpr>,
        then_branch: Box<OptExpr>,
        else_branch: Box<OptExpr>,
    ) -> Rewrite {
        match condition.as_const() {
            Some(Value::Bool(true)) => Rewrite::Changed(*then_branch),
            Some(Value::Bool(false)) | Some(Value::Null) => Rewrite::Changed(*else_branch),
            _ => Rewrite::Same(OptExpr::If {
                id,
                condition,
                then_branch,
                else_branch,
            }),
        }
    }

    fn prune_multi_if(
        &self,
        id: NodeId,
        branches: Vec<(OptExpr, OptExpr)>,
        default: Box<OptExpr>,
    ) -> Rewrite {
        let mut kept = Vec::with_capacity(branches.len());
        let mut new_default: Option<Box<OptExpr>> = None;
        let mut changed = false;

        for (condition, value) in branches {
            match condition.as_const() {
                // A statically-false branch is dead.
                Some(Value::Bool(false)) | Some(Value::Null) => changed = true,
                // A statically-true branch always wins; later branches are dead
                // and its value becomes the effective default.
                Some(Value::Bool(true)) => {
                    new_default = Some(Box::new(value));
                    changed = true;
                    break;
                }
                // Non-constant (or a const non-bool, whose runtime type error we
                // preserve) — keep this branch. Folding continues past it:
                // dropping statically-false branches and truncating at the first
                // statically-true branch is order-independent for `multiIf`, so a
                // later constant branch is still resolved correctly.
                _ => kept.push((condition, value)),
            }
        }

        let default = new_default.unwrap_or(default);

        if kept.is_empty() {
            return Rewrite::Changed(*default);
        }
        if changed {
            Rewrite::Changed(OptExpr::MultiIf {
                id,
                branches: kept,
                default,
            })
        } else {
            Rewrite::Same(OptExpr::MultiIf {
                id,
                branches: kept,
                default,
            })
        }
    }

    fn prune_if_null(&self, id: NodeId, value: Box<OptExpr>, alternative: Box<OptExpr>) -> Rewrite {
        // `ifNull(value, null)` returns `value` in every case — a null `value`
        // yields the null alternative, which equals `value` — so the wrapper is
        // redundant. (This is what collapses an alternative that guard propagation
        // folded to null, e.g. `ifNull(x, upper(x))` → `ifNull(x, null)` → `x`.)
        if matches!(alternative.as_const(), Some(Value::Null)) {
            return Rewrite::Changed(*value);
        }
        match value.as_const() {
            Some(constant) => {
                if constant.is_null() {
                    Rewrite::Changed(*alternative)
                } else {
                    Rewrite::Changed(*value)
                }
            }
            None => Rewrite::Same(OptExpr::IfNull {
                id,
                value,
                alternative,
            }),
        }
    }

    fn prune_null_if(
        &self,
        id: NodeId,
        value: Box<OptExpr>,
        sentinel: Box<OptExpr>,
        cx: &RuleCx,
    ) -> Rewrite {
        match (value.as_const(), sentinel.as_const()) {
            (Some(left), Some(right)) => {
                if left == right {
                    Rewrite::Changed(OptExpr::Const(cx.node_counter.fresh_id(), Value::Null))
                } else {
                    Rewrite::Changed(*value)
                }
            }
            _ => Rewrite::Same(OptExpr::NullIf {
                id,
                value,
                sentinel,
            }),
        }
    }

    /// Fold a `Switch` whose key inputs are all constant: build the dispatch key
    /// exactly as the evaluator does (one input keys on the value, two form a
    /// composite key; an unkeyable value misses) and select the matched branch,
    /// or the default on a miss. The branch value need not be constant, so this is
    /// branch selection (dead-branch elimination), not constant folding.
    fn prune_switch(
        &self,
        id: NodeId,
        inputs: Vec<OptExpr>,
        table: Vec<(Key, OptExpr)>,
        default: Box<OptExpr>,
    ) -> Rewrite {
        let Some(values): Option<Vec<&Value>> = inputs.iter().map(OptExpr::as_const).collect()
        else {
            return Rewrite::Same(OptExpr::Switch {
                id,
                inputs,
                table,
                default,
            });
        };

        let key = if values.len() == 1 {
            Key::from_value(values[0])
        } else {
            Key::composite(values.iter().map(|value| (*value).clone()).collect()).ok()
        };

        let matched = key
            .as_ref()
            .and_then(|key| table.iter().position(|(entry, _)| entry == key));
        match matched {
            Some(position) => {
                let mut table = table;
                let (_, value) = table.swap_remove(position);
                Rewrite::Changed(value)
            }
            None => Rewrite::Changed(*default),
        }
    }

    fn prune_and(
        &self,
        id: NodeId,
        left: Box<OptExpr>,
        right: Box<OptExpr>,
        cx: &RuleCx,
    ) -> Rewrite {
        match left.as_const() {
            Some(Value::Bool(false)) => Rewrite::Changed(OptExpr::Const(
                cx.node_counter.fresh_id(),
                Value::Bool(false),
            )),
            // The right operand becomes the whole result, but the evaluator
            // would still have required it to be Bool/Null — keep that check
            // (a constant Bool/Null right passes it trivially and folds bare).
            Some(Value::Bool(true)) => match right.as_const() {
                Some(Value::Bool(_)) | Some(Value::Null) => Rewrite::Changed(*right),
                _ => Rewrite::Changed(OptExpr::TypeAssert {
                    id: cx.node_counter.fresh_id(),
                    inner: right,
                    expect: TypeClass::Bool,
                    on_present: AssertYield::Identity,
                }),
            },
            Some(Value::Null) => match right.as_const() {
                Some(Value::Bool(false)) => Rewrite::Changed(OptExpr::Const(
                    cx.node_counter.fresh_id(),
                    Value::Bool(false),
                )),
                Some(Value::Bool(true)) | Some(Value::Null) => {
                    Rewrite::Changed(OptExpr::Const(cx.node_counter.fresh_id(), Value::Null))
                }
                _ => Rewrite::Same(OptExpr::And { id, left, right }),
            },
            _ => Rewrite::Same(OptExpr::And { id, left, right }),
        }
    }

    fn prune_or(
        &self,
        id: NodeId,
        left: Box<OptExpr>,
        right: Box<OptExpr>,
        cx: &RuleCx,
    ) -> Rewrite {
        match left.as_const() {
            Some(Value::Bool(true)) => Rewrite::Changed(OptExpr::Const(
                cx.node_counter.fresh_id(),
                Value::Bool(true),
            )),
            // Mirror of `prune_and`: preserve the Bool/Null requirement on the
            // surviving right operand.
            Some(Value::Bool(false)) => match right.as_const() {
                Some(Value::Bool(_)) | Some(Value::Null) => Rewrite::Changed(*right),
                _ => Rewrite::Changed(OptExpr::TypeAssert {
                    id: cx.node_counter.fresh_id(),
                    inner: right,
                    expect: TypeClass::Bool,
                    on_present: AssertYield::Identity,
                }),
            },
            Some(Value::Null) => match right.as_const() {
                Some(Value::Bool(true)) => Rewrite::Changed(OptExpr::Const(
                    cx.node_counter.fresh_id(),
                    Value::Bool(true),
                )),
                Some(Value::Bool(false)) | Some(Value::Null) => {
                    Rewrite::Changed(OptExpr::Const(cx.node_counter.fresh_id(), Value::Null))
                }
                _ => Rewrite::Same(OptExpr::Or { id, left, right }),
            },
            _ => Rewrite::Same(OptExpr::Or { id, left, right }),
        }
    }
}
