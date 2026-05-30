use std::sync::Arc;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::Value;

use crate::cache::ExprCaches;
use crate::error::FuncError;

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

    /// Evaluate the function with concrete values.
    /// Arguments are passed by ownership for zero-copy absorb-when-last optimization.
    fn evaluate(&self, args: Vec<Value>, context: &EvalContext) -> Result<Value, FuncError>;

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

/// Resolves environment variables.
pub trait EnvResolver: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads files for the `file()` function.
pub trait FileResolver: Send + Sync {
    fn read(&self, path: &str, base_dir: &std::path::Path) -> Result<String, FuncError>;
}
