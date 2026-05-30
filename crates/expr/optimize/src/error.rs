use air_elt_commons_arena::{ArenaOverflow, SlicePushError};
use air_elt_expr_funcs::FuncError;
use thiserror::Error;

/// Errors raised while lowering, optimizing, or compacting an expression
/// program into its executable [`crate::CompactProgram`] form.
#[derive(Debug, Error)]
pub enum OptimizeError {
    /// A variable was referenced before any statement bound it.
    #[error("undefined variable: {name}")]
    UndefinedVariable { name: String },

    /// Lowering recursed past the maximum expression nesting depth. Parser-built
    /// programs are already capped, but a directly-constructed `Program` is not,
    /// so the optimizer enforces the bound at its own boundary.
    #[error("expression nesting too deep (max {max})")]
    NestingTooDeep { max: usize },

    /// Resolving a function name (and arity) against the registry failed.
    #[error(transparent)]
    Function(#[from] FuncError),

    /// A fully-constant subexpression in an always-evaluated (eager) position
    /// failed to evaluate at compile time (e.g. `1 / 0`). A constant that
    /// errors is a definite mistake in the source program, so compilation
    /// stops rather than deferring the error to runtime. Errors inside lazy
    /// positions (conditional branches) are left for runtime instead.
    #[error("constant evaluation failed in `{function}`: {error}")]
    ConstEval { function: String, error: String },

    /// A function's constant argument failed static validation — a malformed
    /// inlined format literal (regex pattern, JSONPath, date mask) or a
    /// categorically invalid constant (a shift outside `0..64`, `min > max`, a
    /// non-integer seed). Unlike [`Self::ConstEval`] this is reported in every
    /// position, because such an argument can never be valid regardless of the
    /// path taken.
    #[error("invalid constant argument to `{function}`: {error}")]
    InvalidConstArg { function: String, error: String },

    /// A `field(...)` survived optimization with a non-constant argument. The
    /// column a field reads must be a compile-time-known name; `field("x")` and
    /// the backtick shorthand collapse to a resolved column, so a remaining
    /// `field(<expr>)` means the name is not statically known.
    #[error("field() argument is not a constant column name")]
    NonConstFieldArg,

    /// A `&&` chain asserts the same operand is two incompatible type classes
    /// (e.g. both a string and a bool), so every non-null row would raise a type
    /// error. Such a conjunction can never act as a real predicate — only the
    /// all-null path is error-free — so it is rejected at compile time rather
    /// than failing on the first row of data.
    #[error("infeasible conjunction: an operand is asserted to be both {first} and {second}")]
    InfeasibleConjunction {
        first: &'static str,
        second: &'static str,
    },

    /// The compacted program needed more than `u16::MAX` arena slots.
    #[error("program too large to compact: {0}")]
    Overflow(#[from] ArenaOverflow),

    /// An argument run could not be grown in place during compaction.
    /// This is an internal invariant violation, never user input.
    #[error("internal compaction error: argument run could not be extended ({0})")]
    SliceBuild(#[from] SlicePushError),
}
