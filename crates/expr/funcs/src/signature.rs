use std::sync::Arc;

use air_elt_expr_types::nullable::NullableExprType;
use air_elt_types::Value;

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
    /// const folding). Impure functions (`is_pure() == false`) are
    /// rejected in this mode.
    pub is_compile_time: bool,
}

/// Resolves environment variables.
pub trait EnvResolver: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads files for the `file()` function.
pub trait FileResolver: Send + Sync {
    fn read(&self, path: &str, base_dir: &std::path::Path) -> Result<String, FuncError>;
}
