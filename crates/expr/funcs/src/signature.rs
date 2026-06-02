use std::sync::Arc;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::Value;

use crate::cache::ExprCaches;
use crate::error::FuncError;

/// Owned argument container used to build an [`OwnedArgWindow`] and to collect
/// the variadic tail of a call. Backed by a [`SmallVec`](smallvec::SmallVec)
/// with an inline capacity of four `Value`s, so a fixed-arity collection
/// (almost every call — only the variadic `concat`/`coalesce`/`min`/`max`/
/// `format` exceed four) stays inline with no heap allocation. Calls past the
/// inline capacity spill to the heap exactly as a `Vec` would.
pub type FuncArgVec = smallvec::SmallVec<[Value; 4]>;

/// Position-addressed view of a call's evaluated arguments handed to
/// [`ExprFunction::evaluate`]. The window owns the argument *slots* but not
/// necessarily the values: a slot may alias a constant in the program's const
/// pool, a value still living in the register file, or a freshly-computed
/// sub-expression on the evaluator's reusable scratch stack.
///
/// Functions choose, per argument, between borrowing it ([`read`](Self::read),
/// zero-copy — the common case for read-only functions such as comparisons,
/// hashes, and `length`) and taking ownership of it ([`take`](Self::take),
/// which moves a value the window owns and clones one it merely aliases). The
/// borrow checker forbids holding a `read` borrow across a `take` in the same
/// function; in that rare case clone the borrowed value out first.
pub trait ArgWindow {
    /// Number of arguments in the window.
    fn len(&self) -> usize;

    /// Whether the window holds no arguments.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrow argument `index` without copying. Panics when `index` is out of
    /// range — the optimizer's arity check guarantees `index < len()`.
    fn read(&self, index: usize) -> &Value;

    /// Take ownership of argument `index`. Moves the value when the window owns
    /// it (a scratch sub-expression, or a register at its proven last use) and
    /// clones it when it aliases shared storage (the const pool, or a register
    /// read again later). After a `take` the slot must not be read or taken
    /// again. Panics when `index` is out of range.
    fn take(&mut self, index: usize) -> Value;
}

/// The owned [`ArgWindow`]: it holds every argument value inline in a
/// [`FuncArgVec`]. Used by callers that already materialize their arguments —
/// the heap reference evaluator, compile-time const folding — and by the
/// function unit tests. `take` moves the value out and leaves the slot `Null`
/// (never read again, per the [`ArgWindow::take`] contract).
pub struct OwnedArgWindow {
    values: FuncArgVec,
}

impl OwnedArgWindow {
    /// Wrap already-evaluated argument values into an owned window.
    pub fn create(values: impl Into<FuncArgVec>) -> Self {
        Self {
            values: values.into(),
        }
    }
}

impl ArgWindow for OwnedArgWindow {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn read(&self, index: usize) -> &Value {
        &self.values[index]
    }

    fn take(&mut self, index: usize) -> Value {
        std::mem::replace(&mut self.values[index], Value::Null)
    }
}

/// An [`ArgWindow`] over a **borrowed** slice of already-evaluated argument
/// values — typically a region of a reusable per-program argument stack. Like
/// [`OwnedArgWindow`] but it borrows its storage instead of owning it, so a
/// recursive evaluator that pushes a call's arguments onto a shared stack
/// (truncating back afterwards) pays no per-call allocation. `take` moves the
/// value out and leaves the slot `Null` (never read again, per the contract).
pub struct SliceArgWindow<'a> {
    values: &'a mut [Value],
}

impl<'a> SliceArgWindow<'a> {
    /// Build a window over a slice of already-evaluated argument values.
    pub fn create(values: &'a mut [Value]) -> Self {
        Self { values }
    }
}

impl ArgWindow for SliceArgWindow<'_> {
    fn len(&self) -> usize {
        self.values.len()
    }

    fn read(&self, index: usize) -> &Value {
        &self.values[index]
    }

    fn take(&mut self, index: usize) -> Value {
        std::mem::replace(&mut self.values[index], Value::Null)
    }
}

/// Trait for expression language functions.
/// Each function implementation provides type resolution and evaluation.
pub trait ExprFunction: Send + Sync {
    /// Function name as it appears in expressions (e.g., "concat", "env", "if").
    fn name(&self) -> &str;

    /// Minimum number of arguments.
    fn min_args(&self) -> usize;

    /// Maximum number of arguments (None = variadic/unlimited).
    fn max_args(&self) -> Option<usize>;

    /// Resolve the output type given concrete argument types.
    /// Called at compile-time (type-check pass).
    fn resolve_type(&self, args: &[NullableExprType]) -> Result<NullableExprType, FuncError>;

    /// Evaluate the function over its argument window.
    ///
    /// Arguments are reached through [`ArgWindow`]: borrow a read-only argument
    /// with [`read`](ArgWindow::read) (zero-copy) and take ownership of one the
    /// function consumes with [`take`](ArgWindow::take). The window is `&mut dyn`
    /// so [`ExprFunction`] stays object-safe.
    fn evaluate(&self, args: &mut dyn ArgWindow, context: &EvalContext)
    -> Result<Value, FuncError>;

    /// Whether this function may be evaluated at compile time (const
    /// folding / compile-time context). Default is **`false`** (fail-closed):
    /// a function must explicitly opt into purity. An unmarked function is
    /// merely not const-folded (a missed optimization, never a correctness
    /// bug); impure functions (`now`, `today`, unseeded `random*`) correctly
    /// stay impure by default.
    fn is_pure(&self) -> bool {
        false
    }

    /// Purity refined by which arguments are constant. `const_args[i]` is
    /// `true` when argument `i` folds to a constant. The default ignores
    /// argument constness and defers to [`Self::is_pure`]; functions whose
    /// purity depends on an argument (e.g. `random(min, max, seed)` is pure
    /// only when `seed` is constant) override this.
    fn purity(&self, const_args: &[bool]) -> bool {
        let _ = const_args;
        self.is_pure()
    }

    /// Whether this function may return an error for well-typed arguments
    /// (e.g. `divide`/`modulo` on a zero divisor, `toInt8` on an out-of-range
    /// value, `parseJson` on malformed input). The default is **`true`**
    /// (conservative): a function must explicitly opt out. The optimizer uses
    /// this to decide whether dropping an unread binding is safe — an unread
    /// binding whose value may fail must be kept so its error still surfaces,
    /// matching eager evaluation. Mis-marking a fallible function as
    /// infallible would silently discard a runtime error, so only functions
    /// that are total for every well-typed input return `false`.
    fn can_fail(&self) -> bool {
        true
    }

    /// Statically validate the **constant subset** of a call's arguments.
    /// `args[i]` is `Some(value)` when argument `i` is known at compile time
    /// (a literal or a folded constant) and `None` when it is dynamic. The
    /// optimizer's static-analysis pass calls this for every surviving call.
    ///
    /// Report only **definite, dynamic-data-independent** errors deducible from
    /// what is known: a malformed inlined format literal (regex pattern,
    /// JSONPath, date mask), a categorically invalid constant argument (a shift
    /// outside `0..64`, `min > max`, a non-integer `seed`), or a constant of the
    /// wrong type for a strict slot. Do **not** report failures that depend on a
    /// dynamic operand's value or type (e.g. division by a constant zero — the
    /// outcome depends on the dynamic dividend's type); those are deferred to
    /// the type-aware pass or to runtime.
    ///
    /// The default validates nothing. `context` is provided so an override may
    /// also warm a compiled-artifact cache (e.g. compile a constant regex once).
    fn validate_const_args(
        &self,
        args: &[Option<&Value>],
        context: &EvalContext,
    ) -> Result<(), FuncError> {
        let _ = (args, context);
        Ok(())
    }
}

/// Context available to functions during evaluation.
/// Provides access to side effects (env vars, file reads, current time).
#[derive(Clone)]
pub struct EvalContext {
    pub env_resolver: Arc<dyn EnvResolver>,
    pub file_resolver: Arc<dyn FileResolver>,
    pub now: chrono::DateTime<chrono::Utc>,
    pub base_dir: std::path::PathBuf,
    /// `true` when evaluating in a compile-time context (config patching,
    /// const folding). The type-check pass uses this to reject impure
    /// functions (`is_pure() == false`) in compile-time positions; the
    /// evaluator itself does not gate on it (const folding instead skips
    /// impure calls via [`ExprFunction::purity`]).
    pub is_compile_time: bool,
    /// Per-flow compiled-artifact caches (regex, JSONPath). Built once with the
    /// context and shared across rows; warmed at compile time by
    /// [`ExprFunction::validate_const_args`].
    pub caches: ExprCaches,
}

impl EvalContext {
    /// A copy of this context with `now` overridden — the per-batch runtime
    /// context. The `Arc`-backed `caches` and resolvers are shared by the clone
    /// (cheap), so a constant regex / JSONPath compiled (warmed) at compile time
    /// is reused per row rather than recompiled.
    pub fn with_now(&self, now: chrono::DateTime<chrono::Utc>) -> EvalContext {
        EvalContext {
            now,
            ..self.clone()
        }
    }
}

/// Resolves environment variables.
pub trait EnvResolver: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads files for the `file()` function.
pub trait FileResolver: Send + Sync {
    fn read(&self, path: &str, base_dir: &std::path::Path) -> Result<String, FuncError>;
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct NoEnv;

    impl EnvResolver for NoEnv {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
    }

    struct NoFiles;

    impl FileResolver for NoFiles {
        fn read(&self, _path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
            Ok(String::new())
        }
    }

    #[test]
    fn with_now_overrides_time_and_shares_caches() {
        let base = EvalContext {
            env_resolver: Arc::new(NoEnv),
            file_resolver: Arc::new(NoFiles),
            now: chrono::Utc::now(),
            base_dir: std::path::PathBuf::from("/tmp"),
            is_compile_time: false,
            caches: ExprCaches::default(),
        };
        let later = base.now + chrono::Duration::seconds(60);
        let batch = base.with_now(later);

        assert_eq!(batch.now, later);
        // Resolvers (and, by the same `Arc`-backed `Clone`, the compiled-artifact
        // caches) are shared — not rebuilt — by the per-batch clone.
        assert!(Arc::ptr_eq(&base.env_resolver, &batch.env_resolver));
        assert!(Arc::ptr_eq(&base.file_resolver, &batch.file_resolver));
    }
}
