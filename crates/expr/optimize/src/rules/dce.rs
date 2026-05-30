//! Dead-branch elimination and constant short-circuiting.
//!
//! When a conditional's controlling value is constant, the branch that can
//! never run is dropped. The boolean operators use the same three-valued
//! (`true`/`false`/`null`) semantics as the heap evaluator, so the optimizer
//! and the runtime always agree. These rules assume a well-typed program
//! (conditions and boolean operands are `Bool`/`Null`), which the type-check
//! pass guarantees. A `Switch` whose key inputs are all constant folds to the
//! matched branch (or the default on a miss) — its key is built exactly as the
//! evaluator builds it, so the dispatch is resolved at compile time.

use air_elt_types::{Key, Value};

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct BranchPrune;

impl Rule for BranchPrune {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        match node {
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
            } => self.prune_if(condition, then_branch, else_branch),
            OptExpr::MultiIf { branches, default } => self.prune_multi_if(branches, default),
            OptExpr::IfNull { value, alternative } => self.prune_if_null(value, alternative),
            OptExpr::NullIf { value, sentinel } => self.prune_null_if(value, sentinel),
            OptExpr::And { left, right } => self.prune_and(left, right),
            OptExpr::Or { left, right } => self.prune_or(left, right),
            OptExpr::Switch {
                inputs,
                table,
                default,
            } => self.prune_switch(inputs, table, default),
            other => Rewrite::Same(other),
        }
    }
}

impl BranchPrune {
    fn prune_if(
        &self,
        condition: Box<OptExpr>,
        then_branch: Box<OptExpr>,
        else_branch: Box<OptExpr>,
    ) -> Rewrite {
        match condition.as_const() {
            Some(Value::Bool(true)) => Rewrite::Changed(*then_branch),
            Some(Value::Bool(false)) | Some(Value::Null) => Rewrite::Changed(*else_branch),
            _ => Rewrite::Same(OptExpr::If {
                condition,
                then_branch,
                else_branch,
            }),
        }
    }

    fn prune_multi_if(&self, branches: Vec<(OptExpr, OptExpr)>, default: Box<OptExpr>) -> Rewrite {
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
                branches: kept,
                default,
            })
        } else {
            Rewrite::Same(OptExpr::MultiIf {
                branches: kept,
                default,
            })
        }
    }

    fn prune_if_null(&self, value: Box<OptExpr>, alternative: Box<OptExpr>) -> Rewrite {
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
            None => Rewrite::Same(OptExpr::IfNull { value, alternative }),
        }
    }

    fn prune_null_if(&self, value: Box<OptExpr>, sentinel: Box<OptExpr>) -> Rewrite {
        match (value.as_const(), sentinel.as_const()) {
            (Some(left), Some(right)) => {
                if left == right {
                    Rewrite::Changed(OptExpr::Const(Value::Null))
                } else {
                    Rewrite::Changed(*value)
                }
            }
            _ => Rewrite::Same(OptExpr::NullIf { value, sentinel }),
        }
    }

    /// Fold a `Switch` whose key inputs are all constant: build the dispatch key
    /// exactly as the evaluator does (one input keys on the value, two form a
    /// composite key; an unkeyable value misses) and select the matched branch,
    /// or the default on a miss. The branch value need not be constant, so this is
    /// branch selection (dead-branch elimination), not constant folding.
    fn prune_switch(
        &self,
        inputs: Vec<OptExpr>,
        table: Vec<(Key, OptExpr)>,
        default: Box<OptExpr>,
    ) -> Rewrite {
        let Some(values): Option<Vec<&Value>> = inputs.iter().map(OptExpr::as_const).collect()
        else {
            return Rewrite::Same(OptExpr::Switch {
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

    fn prune_and(&self, left: Box<OptExpr>, right: Box<OptExpr>) -> Rewrite {
        match left.as_const() {
            Some(Value::Bool(false)) => Rewrite::Changed(OptExpr::Const(Value::Bool(false))),
            Some(Value::Bool(true)) => Rewrite::Changed(*right),
            Some(Value::Null) => match right.as_const() {
                Some(Value::Bool(false)) => Rewrite::Changed(OptExpr::Const(Value::Bool(false))),
                Some(Value::Bool(true)) | Some(Value::Null) => {
                    Rewrite::Changed(OptExpr::Const(Value::Null))
                }
                _ => Rewrite::Same(OptExpr::And { left, right }),
            },
            _ => Rewrite::Same(OptExpr::And { left, right }),
        }
    }

    fn prune_or(&self, left: Box<OptExpr>, right: Box<OptExpr>) -> Rewrite {
        match left.as_const() {
            Some(Value::Bool(true)) => Rewrite::Changed(OptExpr::Const(Value::Bool(true))),
            Some(Value::Bool(false)) => Rewrite::Changed(*right),
            Some(Value::Null) => match right.as_const() {
                Some(Value::Bool(true)) => Rewrite::Changed(OptExpr::Const(Value::Bool(true))),
                Some(Value::Bool(false)) | Some(Value::Null) => {
                    Rewrite::Changed(OptExpr::Const(Value::Null))
                }
                _ => Rewrite::Same(OptExpr::Or { left, right }),
            },
            _ => Rewrite::Same(OptExpr::Or { left, right }),
        }
    }
}
