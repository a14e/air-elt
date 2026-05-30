//! Conditional flattening: collapse `if`/`multiIf` chains into one `multiIf`.
//!
//! `if(c, t, <conditional>)` and `multiIf(…, default = <conditional>)` merge the
//! trailing conditional's branches up into the parent, producing a single flat
//! `multiIf`. The rule fires only when the else/default branch is itself an
//! `if`/`multiIf` (a chain) — a standalone `if(c, t, leaf)` is left as an `if`,
//! so the cheaper two-way form is preserved.
//!
//! Type-neutral: both `if` and `multiIf` resolve to the then/first-branch data
//! type and are nullable if any branch is, so merging changes neither. Running
//! bottom-up, deeply-nested chains flatten incrementally. Canonicalizing to one
//! `multiIf` gives uniform dead-branch pruning ([`super::dce`]) and exposes the
//! shape the switch-lowering rule ([`super::switch_lower`]) matches.

use super::{Rewrite, Rule, RuleCx};
use crate::model::opt_expr::OptExpr;

pub(crate) struct FlattenConditionals;

impl Rule for FlattenConditionals {
    fn apply(&self, node: OptExpr, _cx: &RuleCx) -> Rewrite {
        match node {
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
            } => Self::flatten_if(*condition, *then_branch, *else_branch),
            OptExpr::MultiIf { branches, default } => Self::absorb_default(branches, *default),
            other => Rewrite::Same(other),
        }
    }
}

impl FlattenConditionals {
    fn flatten_if(condition: OptExpr, then_branch: OptExpr, else_branch: OptExpr) -> Rewrite {
        match else_branch {
            OptExpr::If {
                condition: else_condition,
                then_branch: else_then,
                else_branch: else_else,
            } => Rewrite::Changed(OptExpr::MultiIf {
                branches: vec![(condition, then_branch), (*else_condition, *else_then)],
                default: else_else,
            }),
            OptExpr::MultiIf {
                mut branches,
                default,
            } => {
                let mut merged = Vec::with_capacity(branches.len() + 1);
                merged.push((condition, then_branch));
                merged.append(&mut branches);
                Rewrite::Changed(OptExpr::MultiIf {
                    branches: merged,
                    default,
                })
            }
            other => Rewrite::Same(OptExpr::If {
                condition: Box::new(condition),
                then_branch: Box::new(then_branch),
                else_branch: Box::new(other),
            }),
        }
    }

    fn absorb_default(mut branches: Vec<(OptExpr, OptExpr)>, default: OptExpr) -> Rewrite {
        match default {
            OptExpr::If {
                condition,
                then_branch,
                else_branch,
            } => {
                branches.push((*condition, *then_branch));
                Rewrite::Changed(OptExpr::MultiIf {
                    branches,
                    default: else_branch,
                })
            }
            OptExpr::MultiIf {
                branches: mut inner_branches,
                default: inner_default,
            } => {
                branches.append(&mut inner_branches);
                Rewrite::Changed(OptExpr::MultiIf {
                    branches,
                    default: inner_default,
                })
            }
            other => Rewrite::Same(OptExpr::MultiIf {
                branches,
                default: Box::new(other),
            }),
        }
    }
}
