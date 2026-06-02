//! The rewrite-rule engine and its registered rules.
//!
//! See [`engine`] for the [`Rule`] trait and the rule set, [`rewrite_driver`]
//! for the bottom-up fixpoint walk that drives them, and the individual rule
//! modules for each transformation.

mod concat_collapse;
mod const_fold;
mod dce;
mod de_morgan;
mod empty_needle;
mod encode_round_trip;
mod engine;
mod field_collapse;
mod flatten;
mod flatten_conditionals;
mod idempotent;
mod multi_if_collapse;
mod object_access;
mod or_membership;
mod rewrite_driver;
mod round_trip;
mod switch_build;
mod switch_collapse;
mod switch_lower;
mod type_assert_collapse;

pub(crate) use engine::{Rewrite, Rule, RuleCx, RuleSet};
pub(crate) use rewrite_driver::RewriteDriver;
