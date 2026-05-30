//! The rewrite-rule engine and its registered rules.
//!
//! See [`engine`] for the [`Rule`] trait and the rule set, [`rewrite_driver`]
//! for the bottom-up fixpoint walk that drives them, and the individual rule
//! modules for each transformation.

mod const_fold;
mod dce;
mod de_morgan;
mod empty_needle;
mod engine;
mod field_collapse;
mod flatten;
mod flatten_conditionals;
mod idempotent;
mod multi_if_collapse;
mod rewrite_driver;
mod round_trip;
mod switch_lower;

pub(crate) use engine::{Rewrite, Rule, RuleCx, RuleSet};
pub(crate) use rewrite_driver::RewriteDriver;
