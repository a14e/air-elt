//! The rewrite-rule engine: the [`Rule`] trait, its outcome type, the shared
//! context, and the registered rule set.
//!
//! Each [`Rule`] takes ownership of one IR node and returns it either rewritten
//! ([`Rewrite::Changed`]) or untouched ([`Rewrite::Same`]). Rules are built once
//! against the registry so they can cache the [`FuncRef`](air_elt_expr_funcs::FuncRef)s
//! of the operators they match. The [`rewrite_driver`](super::rewrite_driver) walks bottom-up and
//! runs the whole set to a local fixpoint at each node.
//!
//! Rules split into two groups. **Fixpoint rules** are **size-non-increasing**:
//! each either shrinks the node count or rewrites to an already-present subtree,
//! which makes the node count a monotone termination measure for the optimizer's
//! outer loop. **Finalize rules** run once in a single bottom-up sweep *after*
//! the fixpoint converges (via
//! [`RewriteDriver`](super::RewriteDriver)'s `finalize`); they invert a
//! fixpoint canonicalization (e.g. `multiIf` → `if`) so they must not feed back
//! into it, and are exempt from the size-non-increasing invariant.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;

use super::{
    concat_collapse, const_fold, dce, de_morgan, empty_needle, encode_round_trip, field_collapse,
    flatten, flatten_conditionals, idempotent, multi_if_collapse, or_membership, round_trip,
    switch_collapse, switch_lower, type_assert_collapse,
};
use crate::model::opt_expr::OptExpr;

/// The outcome of applying a rule to a node.
pub(crate) enum Rewrite {
    /// The rule fired; the node was replaced.
    Changed(OptExpr),
    /// The rule did not apply; the node is returned unchanged.
    Same(OptExpr),
}

/// Shared context a rule may consult while rewriting.
pub(crate) struct RuleCx<'a> {
    pub(crate) registry: &'a FunctionRegistry,
    pub(crate) eval_context: &'a EvalContext,
}

/// A single rewrite rule.
///
/// A rule takes ownership of one node and returns it rewritten
/// ([`Rewrite::Changed`]) or untouched ([`Rewrite::Same`]). Rules never fail:
/// a constant subexpression that cannot be evaluated during folding (e.g.
/// `1 / 0`) is left in place — it may sit in a branch [`dce`] is about to
/// prune. The post-fixpoint static [`check`](crate::check) is the single place
/// that turns a surviving eager constant failure into a compile error.
pub(crate) trait Rule {
    fn apply(&self, node: OptExpr, cx: &RuleCx) -> Rewrite;
}

/// The registered rewrite rules, split into the size-non-increasing rules the
/// driver runs to a local fixpoint at each node and the one-shot finalize rules
/// it sweeps once after the fixpoint converges.
pub(crate) struct RuleSet {
    fixpoint_rules: Vec<Box<dyn Rule>>,
    finalize_rules: Vec<Box<dyn Rule>>,
}

impl RuleSet {
    /// Build the registered rule set. Fixpoint rules are ordered cheapest-first;
    /// const folding runs before the structural rules so a freshly folded
    /// constant is visible to them within the same local-fixpoint sweep. The
    /// `multiIf` → `if` collapse is a finalize rule: it inverts
    /// [`flatten_conditionals`] and so must run once after the fixpoint, never
    /// within it.
    pub(crate) fn create(registry: &FunctionRegistry) -> Self {
        let fixpoint_rules: Vec<Box<dyn Rule>> = vec![
            Box::new(field_collapse::FieldCollapse),
            Box::new(const_fold::ConstFold),
            Box::new(const_fold::InterpolationFold),
            Box::new(const_fold::ObjectFold),
            Box::new(flatten_conditionals::FlattenConditionals),
            Box::new(dce::BranchPrune),
            Box::new(switch_lower::SwitchLower::create(registry)),
            Box::new(switch_collapse::SwitchCollapse),
            Box::new(de_morgan::DeMorgan::create(registry)),
            Box::new(idempotent::IdempotentCollapse::create(registry)),
            Box::new(round_trip::RoundTripCollapse::create(registry)),
            Box::new(encode_round_trip::EncodeRoundTrip::create(registry)),
            Box::new(empty_needle::EmptyNeedle::create(registry)),
            Box::new(flatten::Flatten::create(registry)),
            Box::new(concat_collapse::ConcatCollapse::create(registry)),
            Box::new(concat_collapse::TrimConcat::create(registry)),
            Box::new(type_assert_collapse::TypeAssertCollapse),
        ];
        let finalize_rules: Vec<Box<dyn Rule>> = vec![
            Box::new(multi_if_collapse::MultiIfCollapse),
            Box::new(or_membership::OrMembership::create(registry)),
        ];
        Self {
            fixpoint_rules,
            finalize_rules,
        }
    }

    pub(crate) fn fixpoint_rules(&self) -> &[Box<dyn Rule>] {
        &self.fixpoint_rules
    }

    pub(crate) fn finalize_rules(&self) -> &[Box<dyn Rule>] {
        &self.finalize_rules
    }
}
