use air_elt_commons_arena::{ArenaOverflow, SlicePushError};
use air_elt_expr_funcs::FuncError;
use air_elt_expr_types::error::ExprTypeError;
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

    /// An array literal's element types failed to unify into a single element
    /// type (an incompatible mix, e.g. `[1, "a"]`).
    #[error(transparent)]
    Type(#[from] ExprTypeError),

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

    /// An object literal repeats a key. The parser preserves every entry, so a
    /// duplicate is caught here at conversion (before const-folding can collapse
    /// the literal into an opaque constant) and rejected — a repeated key is
    /// almost always a mistake, and `Value::Object` is an ordered list that would
    /// otherwise silently carry both entries.
    #[error("duplicate key in object literal: {key:?}")]
    DuplicateObjectKey { key: String },

    /// A `field("name")` (or backtick shorthand) references a column absent from
    /// a fixed source schema. Raised only against a
    /// [`Fixed`](air_elt_types::SchemaKind::Fixed) schema — a schemaless source
    /// cannot prove a field absent, so the read stays dynamic there.
    #[error("source field `{name}` is not in the schema")]
    FieldNotInSchema { name: String },

    /// The compiled program's result type cannot produce the expected sink
    /// column type (and `truncate` does not bridge the gap). Caught at compile
    /// time so a mis-typed compute column fails validation before any data moves.
    #[error(
        "output type mismatch: result `{resolved}` is not compatible with expected `{expected}`"
    )]
    OutputTypeMismatch { resolved: String, expected: String },

    /// The compacted program needed more than `u16::MAX` arena slots.
    #[error("program too large to compact: {0}")]
    Overflow(#[from] ArenaOverflow),

    /// An argument run could not be grown in place during compaction.
    /// This is an internal invariant violation, never user input.
    #[error("internal compaction error: argument run could not be extended ({0})")]
    SliceBuild(#[from] SlicePushError),
}
