//! Per-flow compiled-artifact caches for expression evaluation.
//!
//! Regex patterns and JSONPath expressions are expensive to compile and are
//! almost always constant literals. The caches live on
//! [`EvalContext`](crate::signature::EvalContext) so a context built once per
//! flow keeps them hot across rows; cloning the context shares the same store
//! (a [`FifoCache`] is `Arc`-backed). The optimizer additionally warms them at
//! compile time through `validate_const_args`, so a constant pattern compiles
//! once for the whole flow.

use std::str::FromStr;

use air_elt_commons_caching::FifoCache;
use jsonpath_rust::JsonPath;
use regex::Regex;

use crate::error::FuncError;

/// Default per-flow capacity for each artifact cache. A flow's expressions
/// reference only a handful of distinct constant patterns, so this bounds
/// memory while keeping every realistic working set resident.
pub const DEFAULT_EXPR_CACHE_CAPACITY: usize = 64;

/// Compiled-artifact caches threaded through [`EvalContext`].
///
/// The compiled artifacts are stored directly (no `Arc`): callers reach them
/// through the `with_*_cached` methods, which run a closure against a shared
/// reference rather than handing the value out. A cache hit therefore costs a
/// single shared read lock — no reference-count bump, no clone. The store is
/// updated only on a miss, which for a flow's constant patterns happens once
/// (warmed at compile time), so the read-only access path is the hot one.
#[derive(Clone)]
pub struct ExprCaches {
    regex: FifoCache<String, Regex>,
    json_path: FifoCache<String, JsonPath<serde_json::Value>>,
}

impl ExprCaches {
    /// Caches sized to hold `capacity` entries each. `capacity == 0` makes them
    /// pass-through (compile on every call, never store) — used by tests and
    /// throwaway contexts that gain nothing from caching.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            regex: FifoCache::new(capacity),
            json_path: FifoCache::new(capacity),
        }
    }

    /// Compiles `pattern` (cached per flow) and runs `use_regex` against the
    /// shared compiled [`Regex`], returning its result. The regex is borrowed,
    /// never cloned out. A malformed pattern surfaces
    /// [`FuncError::RegexCompileFailed`].
    pub fn with_regex_cached<R>(
        &self,
        pattern: &str,
        use_regex: impl FnOnce(&Regex) -> R,
    ) -> Result<R, FuncError> {
        let build = || {
            Regex::new(pattern).map_err(|err| FuncError::RegexCompileFailed {
                reason: format!("{pattern:?}: {err}"),
            })
        };
        self.regex
            .with_or_try_insert_with(pattern.to_owned(), build, use_regex)
    }

    /// Compiles `path` (cached per flow) and runs `use_path` against the shared
    /// compiled [`JsonPath`], returning its result. A malformed path surfaces
    /// [`FuncError::JsonPathError`].
    pub fn with_json_path_cached<R>(
        &self,
        path: &str,
        use_path: impl FnOnce(&JsonPath<serde_json::Value>) -> R,
    ) -> Result<R, FuncError> {
        let build = || {
            JsonPath::from_str(path).map_err(|err| FuncError::JsonPathError {
                reason: err.to_string(),
            })
        };
        self.json_path
            .with_or_try_insert_with(path.to_owned(), build, use_path)
    }
}

impl Default for ExprCaches {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_EXPR_CACHE_CAPACITY)
    }
}
