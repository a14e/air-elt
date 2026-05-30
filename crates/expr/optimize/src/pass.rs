//! [`Pass`] — the uniform interface for an in-place optimization over the heap
//! [`OptProgram`].
//!
//! Every optimization that mutates an `OptProgram` in place — guard propagation,
//! the rule-driven rewrite fixpoint, the whole-program second pass — implements
//! this one trait instead of carrying its own bespoke verb (`run`,
//! `optimize_program`, `finalize_program`). [`Optimizer`](crate::optimizer) holds
//! the passes as a `Vec<Box<dyn Pass>>` and drives them uniformly: `optimize`
//! once per round of the size-monotone fixpoint loop, then `finalize` once after
//! it converges.
//!
//! The scope is deliberately the *in-place* `OptProgram` passes only. The
//! pipeline-stage engines ([`OptProgramConverter`](crate::engines) and
//! [`Compactor`](crate::engines)) transform *between* program types
//! (`Program → OptProgram → CompactProgram`) rather than rewriting one in place,
//! so they keep their own verbs (`convert` / `compact`) — naming them `optimize`
//! would misdescribe them. The [`MoveAnnotator`](crate::engines) is the same: it
//! rewrites a `CompactProgram` (a different type than `OptProgram`) in place, so
//! it is not a `Pass` and keeps its own verb (`annotate`). The
//! [`ProgramEvaluator`](crate::engines) (`CompactProgram → Value`) is not in the
//! compile pipeline at all — it is the tests' correctness oracle.

use crate::model::opt_program::OptProgram;

/// An in-place optimization over the heap [`OptProgram`].
pub(crate) trait Pass {
    /// Apply the pass once. Must be size-non-increasing so it composes into the
    /// optimizer's monotone fixpoint loop; runs each round.
    fn optimize(&self, program: &mut OptProgram);

    /// One-shot finalization after the fixpoint converges. May grow the program
    /// (e.g. field-read hoisting) or invert a fixpoint canonicalization (e.g.
    /// `multiIf` → `if`). The default is a no-op for passes with nothing to do
    /// once the fixpoint settles.
    fn finalize(&self, _program: &mut OptProgram) {}
}
