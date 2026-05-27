use ahash::AHashMap;
use air_elt_expr_types::limits::RESERVED_CONTROL_FLOW_NAMES;

use crate::error::FuncError;
use crate::signature::ExprFunction;

/// A reference into the function registry's flat array.
/// Represents a slice `functions[start..end]` containing all overloads for a name.
#[derive(Debug, Clone, Copy)]
pub struct FuncRef {
    start: u16,
    end: u16,
}

impl FuncRef {
    pub fn resolve<'a>(
        &self,
        functions: &'a [&'static dyn ExprFunction],
    ) -> &'a [&'static dyn ExprFunction] {
        &functions[self.start as usize..self.end as usize]
    }
}

/// Registry of expression functions, supporting overloading by name.
/// Multiple functions can share a name if they differ in arity.
/// Functions are stored as `&'static dyn ExprFunction` references into a flat array.
pub struct FunctionRegistry {
    names: AHashMap<String, FuncRef>,
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
    /// Overloads for the same name must be registered consecutively.
    pub fn register(&mut self, function: &'static dyn ExprFunction) {
        let name = function.name().to_string();
        assert!(
            !RESERVED_CONTROL_FLOW_NAMES.contains(&name.as_str()),
            "cannot register '{name}' as a function — reserved control flow keyword"
        );
        if let Some(func_ref) = self.names.get_mut(&name) {
            assert_eq!(
                func_ref.end as usize,
                self.functions.len(),
                "overloads for '{name}' must be registered consecutively"
            );
            self.functions.push(function);
            func_ref.end += 1;
        } else {
            let start = self.functions.len() as u16;
            self.functions.push(function);
            self.names.insert(
                name,
                FuncRef {
                    start,
                    end: start + 1,
                },
            );
        }
    }

    /// Look up a FuncRef by name.
    pub fn get_ref(&self, name: &str) -> Option<FuncRef> {
        self.names.get(name).copied()
    }

    /// Resolve the best matching overload for a function call with given argument count.
    pub fn resolve(
        &self,
        name: &str,
        arg_count: usize,
    ) -> Result<&'static dyn ExprFunction, FuncError> {
        let func_ref = self
            .names
            .get(name)
            .ok_or_else(|| FuncError::UnknownFunction {
                name: name.to_string(),
            })?;

        let overloads = &self.functions[func_ref.start as usize..func_ref.end as usize];

        let matching: Vec<_> = overloads
            .iter()
            .filter(|f| {
                arg_count >= f.min_args() && f.max_args().is_none_or(|max| arg_count <= max)
            })
            .collect();

        match matching.len() {
            0 => Err(FuncError::ArityMismatch {
                function: name.to_string(),
                expected: format_arity_options(overloads),
                actual: arg_count,
            }),
            1 => Ok(*matching[0]),
            _ => Err(FuncError::AmbiguousOverload {
                function: name.to_string(),
                arg_types: format!("{arg_count} arguments"),
            }),
        }
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

    #[test]
    fn register_and_resolve() {
        let mut registry = FunctionRegistry::new();
        registry.register(&TEST_FN);

        let func = registry.resolve("test_fn", 1).unwrap();
        assert_eq!(func.name(), "test_fn");
    }

    #[test]
    fn unknown_function_errors() {
        let registry = FunctionRegistry::new();
        let result = registry.resolve("nonexistent", 0);
        assert!(matches!(result, Err(FuncError::UnknownFunction { .. })));
    }

    #[test]
    fn arity_mismatch_errors() {
        let mut registry = FunctionRegistry::new();
        registry.register(&FIXED);

        let result = registry.resolve("fixed", 5);
        assert!(matches!(result, Err(FuncError::ArityMismatch { .. })));
    }

    #[test]
    fn variadic_function_accepts_many_args() {
        let mut registry = FunctionRegistry::new();
        registry.register(&VARIADIC);

        assert!(registry.resolve("variadic", 1).is_ok());
        assert!(registry.resolve("variadic", 10).is_ok());
        assert!(registry.resolve("variadic", 100).is_ok());
        assert!(registry.resolve("variadic", 0).is_err());
    }
}
