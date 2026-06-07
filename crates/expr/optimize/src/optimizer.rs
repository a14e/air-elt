//! The top-level optimization pipeline. [`Optimizer::optimize`] runs
//! convert → fixpoint → finalize → static-check and yields an [`OptProgram`];
//! [`Optimizer::compile`] drives it on the typed path (a schema or expected output
//! is supplied) — interleaving the typed rewrites into the fixpoint — then
//! compacts the result into a [`CompactProgram`] and annotates register moves.
//!
//! [`Optimizer`] owns the registry, evaluation context, and node-id counter, and
//! orchestrates the [`Pass`](crate::pass::Pass)es. Each round of the fixpoint loop
//! runs them in order: guard propagation
//! ([`GuardPropagation`](crate::engines::GuardPropagation)), the bottom-up
//! expression rewrite ([`RewriteDriver`](crate::rules::RewriteDriver) over the
//! [`RuleSet`](crate::rules::RuleSet)), and the whole-program second pass
//! ([`SecondPassDriver`](crate::second_pass_rules::SecondPassDriver)); on the typed
//! path the round then re-derives the static type map and applies the
//! [`typed`](crate::typed) rewrites, so typed and untyped simplifications feed each
//! other. The untyped passes are size-non-increasing, so the program measure (node
//! count plus statement count) decreases monotonically; the loop stops once a
//! round neither shrinks the program nor lets a (possibly size-neutral) typed
//! rewrite fire, with a hard cap as backstop. After it converges each pass's
//! one-shot finalizer runs, then the static check.

use air_elt_expr_funcs::FunctionRegistry;
use air_elt_expr_funcs::signature::EvalContext;
use air_elt_expr_parse::model::Program;
use air_elt_types::Schema;

use crate::check::StaticCheckEngine;
use crate::engines::{
    Compactor, ExpectedOutput, GuardPropagation, MoveAnnotator, OptProgramConverter, TypeChecker,
};
use crate::error::OptimizeError;
use crate::model::CompactProgram;
use crate::model::node_id::NodeCounter;
use crate::model::opt_program::OptProgram;
use crate::pass::Pass;
use crate::rules::{RewriteDriver, RuleSet};
use crate::second_pass_rules::{SecondPassDriver, SecondPassSet};
use crate::typed::{TypedRewriteDriver, TypedRuleSet};

const MAX_PROGRAM_ITERS: usize = 16;

/// The optimizing compiler. Holds the function registry, the evaluation context
/// used for compile-time constant folding, and the node-id counter threaded
/// through every node-constructing stage.
pub struct Optimizer<'a> {
    registry: &'a FunctionRegistry,
    eval_context: &'a EvalContext,
    node_counter: NodeCounter,
}

impl<'a> Optimizer<'a> {
    pub fn create(registry: &'a FunctionRegistry, eval_context: &'a EvalContext) -> Self {
        Self {
            registry,
            eval_context,
            node_counter: NodeCounter::create(),
        }
    }

    /// Lower, optimize, and compact a parsed program into its executable form,
    /// then annotate register last-uses as moves.
    ///
    /// `schema` and `expected` drive the static **type** pass (Phase 3b): when
    /// either is present, the optimized program is type-checked before
    /// compaction — `SourceField`s are typed from `schema` (a field absent from a
    /// *fixed* schema is an error), and when `expected` is set the result type is
    /// validated against the sink column's type (honouring `truncate`). With both
    /// `None` (the schemaless / comptime `ConfigExprPatcher` path) the type pass
    /// is skipped and the program's surviving `TypeAssert`s do the per-row
    /// checking, exactly as before.
    pub fn compile(
        &self,
        program: &Program,
        schema: Option<&Schema>,
        expected: Option<&ExpectedOutput>,
    ) -> Result<CompactProgram, OptimizeError> {
        // With a schema, the optimize fixpoint interleaves the typed rewrites
        // (deriving the static type map each round); without one the typed stage is
        // skipped and the surviving `TypeAssert`s do the per-row checking.
        let optimized = self.optimize(program, true, schema)?;
        // Final strict type-check on the CONVERGED tree (the per-round derivation
        // inside the loop is tolerant — it leaves not-yet-pruned dead subtrees
        // unknown rather than raising). This is where the real type errors
        // (absent field, type mismatch, non-const field arg) and the output-type
        // compatibility against the sink column are raised.
        if schema.is_some() || expected.is_some() {
            TypeChecker::create(self.registry, schema, optimized.register_count, false)
                .check(&optimized, expected)?;
        }
        let mut compact = Compactor::create().compact(optimized)?;
        MoveAnnotator::create().annotate(&mut compact);
        Ok(compact)
    }

    /// Convert a parsed program into the optimization IR and, when `apply_rules`
    /// is set, run it to a fixpoint, apply the one-shot finalizers, and run the
    /// static-analysis pass. The node-id counter (`self.node_counter`) is threaded
    /// through every node-constructing stage (converter, rewrite rules, second
    /// pass, guard propagation); the later type-check and typed passes mint nothing.
    pub(crate) fn optimize(
        &self,
        program: &Program,
        apply_rules: bool,
        schema: Option<&Schema>,
    ) -> Result<OptProgram, OptimizeError> {
        let node_counter = &self.node_counter;
        let mut optimized =
            OptProgramConverter::create(self.registry, node_counter).convert(program)?;
        if !apply_rules {
            return Ok(optimized);
        }

        let rule_set = RuleSet::create(self.registry);
        let second_pass_set = SecondPassSet::create();
        // The typed rules interleave into the fixpoint only when a schema is
        // present (the type map is derived against it each round); built once here.
        let typed_rules = schema.map(|_| TypedRuleSet::create(self.registry));
        let static_check = StaticCheckEngine::create(self.registry, self.eval_context);

        // The in-place passes share the `Pass` interface and run in this order
        // each round: guard propagation substitutes operands their branch guards
        // pin to a constant; the bottom-up rewrite then folds the freshly
        // constant subtrees (leaving any that error in place for the static
        // check); the second pass inlines constants and prunes registers.
        let passes: Vec<Box<dyn Pass + '_>> = vec![
            Box::new(GuardPropagation::create(self.registry, node_counter)),
            Box::new(RewriteDriver::create(
                &rule_set,
                self.registry,
                self.eval_context,
                node_counter,
            )),
            Box::new(SecondPassDriver::create(
                &second_pass_set,
                self.registry,
                node_counter,
            )),
        ];

        // The fixpoint interleaves the untyped passes with the typed rewrites (when
        // a type stage is present): each round the untyped passes run, then the
        // type map is re-derived over the freshly-rewritten tree and the typed
        // rules fire — so a typed rewrite (e.g. `add → concat`, typed const-fold)
        // feeds the next round's untyped passes and guard propagation, and vice
        // versa. The map is re-derived per round because the untyped passes mint new
        // node ids; deriving it is one cheap bottom-up walk. The loop ends when a
        // round shrinks the program no further AND the typed rules changed nothing
        // (a typed rewrite can be size-neutral, like `add → concat`, yet still be
        // progress, so size alone is not the termination signal).
        for _ in 0..MAX_PROGRAM_ITERS {
            let before = count_program_items(&optimized);
            for pass in &passes {
                pass.optimize(&mut optimized);
            }
            let typed_changed = match &typed_rules {
                Some(rule_set) => {
                    let mut type_map =
                        TypeChecker::create(self.registry, schema, optimized.register_count, true)
                            .check(&optimized, None)?;
                    let driver = TypedRewriteDriver::create(rule_set, self.registry);
                    let (rewritten, changed) = driver.run(optimized, &mut type_map);
                    optimized = rewritten;
                    changed
                }
                None => false,
            };
            if count_program_items(&optimized) >= before && !typed_changed {
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
