//! Expression optimizer: lowers a parsed program into an optimized, compacted
//! IR ready for fast per-row execution.
//!
//! The pipeline is
//! `convert → fixpoint → finalize → check → [type-check → discharge] → compact →
//! annotate` (the bracketed typed steps run only when a schema or expected output
//! is supplied):
//! * **convert** resolves function names/arities to registry references and
//!   variables to register slots ([`engines::opt_program_converter`]).
//! * **fixpoint** rewrites the heap IR to a fixpoint — downward guard
//!   propagation, const folding, dead-branch elimination, De Morgan factoring,
//!   idempotent/round-trip collapse to `TypeAssert`, associative flattening,
//!   `field` collapse, constant inlining, unused-register pruning. Each in-place
//!   step is a [`Pass`](pass::Pass) ([`rules`], [`engines`], [`optimizer`]).
//! * **finalize** runs each pass's one-shot finalizer once after the fixpoint
//!   converges — node-shape inversions (`multiIf` → `if`) and size-increasing
//!   whole-program steps (field-read hoisting) that cannot join the monotone loop.
//! * **check** runs the static-analysis pass over the optimized program
//!   ([`check`]).
//! * **type-check** (typed path only) derives the static type map
//!   (`AHashMap<NodeId, Type>`) over the heap IR ([`engines::type_check`]).
//! * **typed rewrites** (typed path only, interleaved into the fixpoint) apply
//!   the simplifications a known type makes sound — stripping redundant
//!   `TypeAssert`s and casts, `min`/`max`/`concat` flatten, string `+` → `concat`,
//!   identity/annihilation peepholes, and power reduction ([`typed`]).
//! * **compact** lays the result into arenas in execution order with an interned
//!   constant pool ([`engines::compact`]).
//! * **annotate** marks each register's last read as a move
//!   ([`engines::move_annotator`]).
//!
//! The standalone whole-program engines (convert / guard propagation / compact /
//! move annotation) live under [`engines`]; the rule-based engines live with
//! their rules ([`rules`], [`second_pass_rules`], [`check`]); every in-place
//! optimization shares the [`Pass`](pass::Pass) interface; [`optimizer`] is the
//! orchestrator. [`ProgramEvaluator`](engines::ProgramEvaluator) is not a
//! pipeline stage — it is the field-free correctness oracle that runs a
//! `CompactProgram` so the tests can prove compaction (and the optimizations
//! feeding it) meaning-preserving.
//!
//! Only the entry point ([`Optimizer::compile`]), the executable output
//! ([`CompactProgram`] / [`OptNode`]), and the correctness oracle
//! ([`ProgramEvaluator`]) are public; the heap optimization IR is an internal
//! detail.

pub(crate) mod check;
pub(crate) mod engines;
pub mod error;
pub mod model;
pub mod optimizer;
pub(crate) mod pass;
pub(crate) mod rules;
pub(crate) mod second_pass_rules;
pub(crate) mod typed;
pub(crate) mod util;

#[cfg(test)]
mod test_utils;

pub use engines::{EvalError, ExpectedOutput, FieldSource, ProgramEvaluator};
pub use error::OptimizeError;
pub use model::{
    ArgSlice, CompactProgram, CompactStatement, CompactYield, ConstId, KeyId, KeySlice, NodeRef,
    OptNode, RegisterId, SwitchTable, SwitchTableId, TypeClass,
};
pub use optimizer::Optimizer;

// Re-exported so downstream crates can traverse a `CompactProgram`'s arena
// references without depending on the arena crate directly.
pub use air_elt_commons_arena::{ArenaRef, ArenaSlice};
