//! The top-level optimization pipeline. [`Optimizer::optimize`] runs
//! convert → fixpoint → finalize → static-check and yields an [`OptProgram`];
//! [`Optimizer::compile`] adds the compaction into a [`CompactProgram`] and the
//! register move annotation.
//!
//! [`Optimizer`] owns the registry and evaluation context and orchestrates the
//! [`Pass`](crate::pass::Pass)es. Each round of the fixpoint loop runs them in
//! order: guard propagation
//! ([`GuardPropagation`](crate::engines::GuardPropagation)), the bottom-up
//! expression rewrite ([`RewriteDriver`](crate::rules::RewriteDriver) over the
//! [`RuleSet`](crate::rules::RuleSet)), and the whole-program second pass
//! ([`SecondPassDriver`](crate::second_pass_rules::SecondPassDriver)). Every pass
//! is size-non-increasing, so the program measure (total node count plus
//! statement count) decreases monotonically; the loop stops as soon as a round
//! makes no progress, with a hard cap as backstop. After it converges each pass's
//! one-shot finalizer runs, then the static check.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_parse::model::Program;

use crate::check::StaticCheckEngine;
use crate::engines::{Compactor, GuardPropagation, MoveAnnotator, OptProgramConverter};
use crate::error::OptimizeError;
use crate::model::CompactProgram;
use crate::model::opt_program::OptProgram;
use crate::pass::Pass;
use crate::rules::{RewriteDriver, RuleSet};
use crate::second_pass_rules::{SecondPassDriver, SecondPassSet};

const MAX_PROGRAM_ITERS: usize = 16;

/// The optimizing compiler. Holds the function registry and the evaluation
/// context used for compile-time constant folding.
pub struct Optimizer<'a> {
    registry: &'a FunctionRegistry,
    eval_context: &'a EvalContext,
}

impl<'a> Optimizer<'a> {
    pub fn create(registry: &'a FunctionRegistry, eval_context: &'a EvalContext) -> Self {
        Self {
            registry,
            eval_context,
        }
    }

    /// Lower, optimize, and compact a parsed program into its executable form,
    /// then annotate register last-uses as moves.
    pub fn compile(&self, program: &Program) -> Result<CompactProgram, OptimizeError> {
        let optimized = self.optimize(program, true)?;
        let mut compact = Compactor::create().compact(optimized)?;
        MoveAnnotator::create().annotate(&mut compact);
        Ok(compact)
    }

    /// Convert a parsed program into the optimization IR and, when `apply_rules`
    /// is set, run it to a fixpoint, apply the one-shot finalizers, and run the
    /// static-analysis pass. Exposed within the crate so tests can compare an
    /// optimized program against a merely-converted one (`apply_rules = false`).
    pub(crate) fn optimize(
        &self,
        program: &Program,
        apply_rules: bool,
    ) -> Result<OptProgram, OptimizeError> {
        let mut optimized = OptProgramConverter::create(self.registry).convert(program)?;
        if !apply_rules {
            return Ok(optimized);
        }

        let rule_set = RuleSet::create(self.registry);
        let second_pass_set = SecondPassSet::create();
        let static_check = StaticCheckEngine::create(self.registry, self.eval_context);

        // The in-place passes share the `Pass` interface and run in this order
        // each round: guard propagation substitutes operands their branch guards
        // pin to a constant; the bottom-up rewrite then folds the freshly
        // constant subtrees (leaving any that error in place for the static
        // check); the second pass inlines constants and prunes registers.
        let passes: Vec<Box<dyn Pass + '_>> = vec![
            Box::new(GuardPropagation::create(self.registry)),
            Box::new(RewriteDriver::create(
                &rule_set,
                self.registry,
                self.eval_context,
            )),
            Box::new(SecondPassDriver::create(&second_pass_set, self.registry)),
        ];

        for _ in 0..MAX_PROGRAM_ITERS {
            let before = count_program_items(&optimized);
            for pass in &passes {
                pass.optimize(&mut optimized);
            }
            if count_program_items(&optimized) >= before {
                break;
            }
        }
        // One-shot finalization runs after the fixpoint converges, in the same
        // pass order: the rewrite driver's node-shape finalizers (e.g. `multiIf`
        // → `if` collapse) invert a fixpoint canonicalization and so must not
        // feed back into it; the second pass's whole-program finalizers (e.g.
        // field-read CSE) may grow the program. Neither can take part in the
        // size-non-increasing loop above. (Guard propagation has no finalizer —
        // the default no-op.)
        for pass in &passes {
            pass.finalize(&mut optimized);
        }

        // Dead branches are gone and literals are folded: run the static-analysis
        // pass over what survives — eager constant failures, malformed inlined
        // literals / invalid constant arguments, and non-constant `field` names.
        static_check.check(&optimized)?;

        Ok(optimized)
    }
}

/// Termination measure: total node count plus the number of live statements.
/// Every pass leaves it non-increasing.
fn count_program_items(program: &OptProgram) -> usize {
    let statement_nodes: usize = program
        .statements
        .iter()
        .map(|statement| statement.value.node_count())
        .sum();
    statement_nodes + program.result.node_count() + program.statements.len()
}
