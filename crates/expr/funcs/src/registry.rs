use ahash::AHashMap;
use air_elt_expr_types::limits::RESERVED_CONTROL_FLOW_NAMES;

use crate::error::FuncError;
use crate::signature::ExprFunction;

/// A resolved reference to a single function in the registry's flat array.
///
/// Arity selection happens once at resolution time
/// ([`FunctionRegistry::get_ref`]); the resolved index travels into the
/// optimized program so the runtime never re-resolves by name or arity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuncRef(u16);

/// Internal: the contiguous range of overloads registered under one name.
/// Overloads for a name are stored consecutively in `functions`.
#[derive(Debug, Clone, Copy)]
struct OverloadRange {
    start: u16,
    end: u16,
}

/// Registry of expression functions, supporting overloading by name.
/// Multiple functions can share a name if they differ in arity.
/// Functions are stored as `&'static dyn ExprFunction` references into a flat array.
pub struct FunctionRegistry {
    names: AHashMap<String, OverloadRange>,
    functions: Vec<&'static dyn ExprFunction>,
}

impl FunctionRegistry {
    pub fn new() -> Self {
        Self {
            names: AHashMap::new(),
            functions: Vec::new(),
        }
    }

    /// Register a static function reference.
    ///
    /// Overloads for the same name must be registered consecutively, and
    /// must not share or overlap an arity range with an existing overload
    /// (two `[min,max]` ranges intersect, treating `max_args == None` as
    /// unbounded) — that would make arity-based resolution ambiguous, so
    /// it panics at registration time.
    pub fn register(&mut self, function: &'static dyn ExprFunction) {
        let name = function.name().to_string();
        assert!(
            !RESERVED_CONTROL_FLOW_NAMES.contains(&name.as_str()),
            "cannot register '{name}' as a function — reserved control flow keyword"
        );
        if let Some(range) = self.names.get_mut(&name) {
            assert_eq!(
                range.end as usize,
                self.functions.len(),
                "overloads for '{name}' must be registered consecutively"
            );
            for existing in &self.functions[range.start as usize..range.end as usize] {
                assert!(
                    !arity_ranges_overlap(*existing, function),
                    "cannot register '{name}' — its arity range overlaps an existing overload"
                );
            }
            self.functions.push(function);
            range.end += 1;
        } else {
            let start = self.functions.len() as u16;
            self.functions.push(function);
            self.names.insert(
                name,
                OverloadRange {
                    start,
                    end: start + 1,
                },
            );
        }
    }

    /// Resolve a function name (and optional arity) to a single
    /// [`FuncRef`].
    ///
    /// * `arity = Some(n)` selects the overload whose `[min_args,max_args]`
    ///   range admits `n` arguments.
    /// * `arity = None` selects the variadic overload (`max_args == None`)
    ///   — used by optimizer rules that need an operator's `FuncRef`
    ///   without binding to a concrete argument count.
    pub fn get_ref(&self, name: &str, arity: Option<usize>) -> Result<FuncRef, FuncError> {
        let range = self
            .names
            .get(name)
            .ok_or_else(|| FuncError::UnknownFunction {
                name: name.to_string(),
            })?;
        let (start, end) = (range.start as usize, range.end as usize);
        let overloads = &self.functions[start..end];

        let matched = match arity {
            Some(n) => overloads
                .iter()
                .position(|f| n >= f.min_args() && f.max_args().is_none_or(|max| n <= max)),
            None => overloads.iter().position(|f| f.max_args().is_none()),
        };

        match matched {
            Some(offset) => Ok(FuncRef((start + offset) as u16)),
            None => Err(FuncError::ArityMismatch {
                function: name.to_string(),
                expected: format_arity_options(overloads),
                actual: arity.unwrap_or(0),
            }),
        }
    }

    /// Dereference a resolved [`FuncRef`] to its function.
    pub fn get_by_ref(&self, func_ref: FuncRef) -> &'static dyn ExprFunction {
        self.functions[func_ref.0 as usize]
    }

    /// Create a registry pre-loaded with all builtin functions.
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        crate::builtins::register_builtins(&mut registry);
        registry
    }

    /// Number of registered function names.
    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

impl Default for FunctionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether two functions' `[min_args, max_args]` arity ranges intersect
/// (treating `max_args == None` as unbounded). Overlapping ranges make
/// arity-based overload resolution ambiguous.
fn arity_ranges_overlap(a: &dyn ExprFunction, b: &dyn ExprFunction) -> bool {
    let a_max = a.max_args().unwrap_or(usize::MAX);
    let b_max = b.max_args().unwrap_or(usize::MAX);
    a.min_args() <= b_max && b.min_args() <= a_max
}

fn format_arity_options(overloads: &[&'static dyn ExprFunction]) -> String {
    overloads
        .iter()
        .map(|f| match f.max_args() {
            Some(max) if max == f.min_args() => format!("{}", f.min_args()),
            Some(max) => format!("{}-{}", f.min_args(), max),
            None => format!("{}+", f.min_args()),
        })
        .collect::<Vec<_>>()
        .join(" or ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_expr_types::nullable::NullableExprType;
    use air_elt_types::{DataType, Value};

    use crate::signature::{EvalContext, ExprFunction};

    struct DummyFunc {
        name: &'static str,
        min_args: usize,
        max_args: Option<usize>,
    }

    impl ExprFunction for DummyFunc {
        fn name(&self) -> &str {
            self.name
        }

        fn min_args(&self) -> usize {
            self.min_args
        }

        fn max_args(&self) -> Option<usize> {
            self.max_args
        }

        fn resolve_type(&self, _args: &[NullableExprType]) -> Result<NullableExprType, FuncError> {
            Ok(NullableExprType::non_null(DataType::Int64))
        }

        fn evaluate(&self, _args: Vec<Value>, _context: &EvalContext) -> Result<Value, FuncError> {
            Ok(Value::Int64(42))
        }
    }

    static TEST_FN: DummyFunc = DummyFunc {
        name: "test_fn",
        min_args: 1,
        max_args: Some(1),
    };

    static FIXED: DummyFunc = DummyFunc {
        name: "fixed",
        min_args: 2,
        max_args: Some(2),
    };

    static VARIADIC: DummyFunc = DummyFunc {
        name: "variadic",
        min_args: 1,
        max_args: None,
    };

    static TEST_FN_DUP: DummyFunc = DummyFunc {
        name: "test_fn",
        min_args: 1,
        max_args: Some(1),
    };

    #[test]
    fn register_and_resolve() {
        let mut registry = FunctionRegistry::new();
        registry.register(&TEST_FN);

        let func_ref = registry.get_ref("test_fn", Some(1)).unwrap();
        assert_eq!(registry.get_by_ref(func_ref).name(), "test_fn");
    }

    #[test]
    fn unknown_function_errors() {
        let registry = FunctionRegistry::new();
        let result = registry.get_ref("nonexistent", Some(0));
        assert!(matches!(result, Err(FuncError::UnknownFunction { .. })));
    }

    #[test]
    fn arity_mismatch_errors() {
        let mut registry = FunctionRegistry::new();
        registry.register(&FIXED);

        let result = registry.get_ref("fixed", Some(5));
        assert!(matches!(result, Err(FuncError::ArityMismatch { .. })));
    }

    #[test]
    fn variadic_function_accepts_many_args() {
        let mut registry = FunctionRegistry::new();
        registry.register(&VARIADIC);

        assert!(registry.get_ref("variadic", Some(1)).is_ok());
        assert!(registry.get_ref("variadic", Some(10)).is_ok());
        assert!(registry.get_ref("variadic", Some(100)).is_ok());
        assert!(registry.get_ref("variadic", Some(0)).is_err());
        // `None` arity selects the variadic overload directly.
        assert!(registry.get_ref("variadic", None).is_ok());
    }

    #[test]
    fn overlapping_arity_panics() {
        let result = std::panic::catch_unwind(|| {
            let mut registry = FunctionRegistry::new();
            registry.register(&TEST_FN); // test_fn: 1..=1
            registry.register(&TEST_FN_DUP); // test_fn: 1..=1 again — overlaps
        });
        assert!(
            result.is_err(),
            "expected overlapping-arity registration to panic"
        );
    }

    /// Fail-closed purity contract, resolved end-to-end through the real
    /// builtin registry: pure functions opt into `is_pure`, clock-dependent
    /// and unseeded-random functions stay impure (the default).
    #[test]
    fn builtin_purity_classification() {
        let registry = FunctionRegistry::with_builtins();
        let is_pure = |name: &str, arity: usize| {
            let r = registry
                .get_ref(name, Some(arity))
                .expect("builtin must exist");
            registry.get_by_ref(r).is_pure()
        };
        // Pure builtins explicitly opt in.
        assert!(is_pure("add", 2));
        assert!(is_pure("concat", 2));
        assert!(is_pure("upper", 1));
        assert!(is_pure("toInt64", 1));
        assert!(is_pure("addDays", 2));
        assert!(is_pure("regexMatch", 2));
        // Clock-dependent → impure.
        assert!(!is_pure("now", 0));
        assert!(!is_pure("today", 0));
        // Random without a constant seed → impure (bare `is_pure`).
        assert!(!is_pure("randomInt", 2));
        assert!(!is_pure("randomUuid", 0));
    }
}
