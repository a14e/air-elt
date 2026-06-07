//! The second-pass rule engine: program-level passes that run *after* the
//! expression-rewrite (first) pass.
//!
//! Where the first pass ([`crate::rules`]) rewrites individual IR nodes, a
//! second-pass rule transforms the program as a whole — inlining a constant
//! binding across all its uses, dropping unread registers, hoisting repeated
//! field reads. Passes split in two: size-non-increasing ones compose into the
//! optimizer's monotone fixpoint loop, while size-increasing finalizers run once
//! after it converges. [`SecondPassDriver`] runs each group in registration
//! order, mirroring how [`RewriteDriver`](crate::rules::RewriteDriver) runs the
//! first-pass [`RuleSet`](crate::rules::RuleSet).

use air_elt_expr_funcs::FunctionRegistry;

use super::constant_inliner::ConstantInliner;
use super::field_hoist::FieldHoister;
use super::register_pruner::RegisterPruner;
use crate::model::node_id::NodeCounter;
use crate::model::opt_program::OptProgram;
use crate::pass::Pass;

/// A whole-program rewrite applied after the node-rewrite pass. The registry is
/// available so a pass can consult per-function properties (e.g.
/// [`can_fail`](air_elt_expr_funcs::ExprFunction::can_fail)).
///
/// Fixpoint passes (run by [`SecondPassDriver::optimize`]) must be
/// size-non-increasing so they compose into the optimizer's monotone loop.
/// Finalization passes (run once by [`SecondPassDriver::finalize`]) may grow
/// the program — they execute after the fixpoint has converged.
pub(crate) trait ProgramPass {
    fn run(
        &self,
        program: &mut OptProgram,
        registry: &FunctionRegistry,
        node_counter: &NodeCounter,
    );
}

/// The registered second-pass program rules, split into the size-non-increasing
/// passes that run inside the fixpoint and the one-shot finalization passes that
/// run after it converges.
pub(crate) struct SecondPassSet {
    passes: Vec<Box<dyn ProgramPass>>,
    finalizers: Vec<Box<dyn ProgramPass>>,
}

impl SecondPassSet {
    /// Build the registered second-pass set. Constant inlining runs before
    /// register pruning so freshly-orphaned registers are pruned in the same
    /// round. Field-read hoisting is a size-increasing finalization, so it runs
    /// once after the fixpoint rather than within it.
    pub(crate) fn create() -> Self {
        let passes: Vec<Box<dyn ProgramPass>> =
            vec![Box::new(ConstantInliner), Box::new(RegisterPruner)];
        let finalizers: Vec<Box<dyn ProgramPass>> = vec![Box::new(FieldHoister)];
        Self { passes, finalizers }
    }

    pub(crate) fn passes(&self) -> &[Box<dyn ProgramPass>] {
        &self.passes
    }

    pub(crate) fn finalizers(&self) -> &[Box<dyn ProgramPass>] {
        &self.finalizers
    }
}

/// Runs the second-pass rule set over a program. Holds the registry the inner
/// passes consult, so it satisfies the registry-free [`Pass`] interface.
pub(crate) struct SecondPassDriver<'a> {
    passes: &'a SecondPassSet,
    registry: &'a FunctionRegistry,
    node_counter: &'a NodeCounter,
}

impl<'a> SecondPassDriver<'a> {
    pub(crate) fn create(
        passes: &'a SecondPassSet,
        registry: &'a FunctionRegistry,
        node_counter: &'a NodeCounter,
    ) -> Self {
        Self {
            passes,
            registry,
            node_counter,
        }
    }
}

impl Pass for SecondPassDriver<'_> {
    /// Apply every size-non-increasing second-pass rule once, in registration
    /// order. Called each round of the fixpoint.
    fn optimize(&self, program: &mut OptProgram) {
        for pass in self.passes.passes() {
            pass.run(program, self.registry, self.node_counter);
        }
    }

    /// Apply the one-shot finalization passes once, after the fixpoint has
    /// converged. These may grow the program.
    fn finalize(&self, program: &mut OptProgram) {
        for pass in self.passes.finalizers() {
            pass.run(program, self.registry, self.node_counter);
        }
    }
}
