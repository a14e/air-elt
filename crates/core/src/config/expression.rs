use std::path::Path;
use std::sync::Arc;

use air_elt_expr::{eval_expression, eval_interpolated, has_interpolation, is_expression};
use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
use air_elt_expr_funcs::{FuncError, FunctionRegistry};
use air_elt_types::Value;

use crate::error::ConfigError;

/// Bundles a [`FunctionRegistry`] and [`EvalContext`] for expression
/// evaluation during flow plan construction. Stored on `AssembledFlow`
/// so both the initial validation pass and the runner's rebuild path
/// can evaluate `default = "env('KEY', 'fallback')"` style literals.
#[derive(Clone)]
pub struct ExpressionContext {
    pub registry: Arc<FunctionRegistry>,
    pub eval_context: EvalContext,
}

impl ExpressionContext {
    pub fn new(registry: Arc<FunctionRegistry>, config_dir: &Path) -> Self {
        Self {
            registry,
            eval_context: build_eval_context(config_dir),
        }
    }
}

/// Resolve a TOML value that may contain expressions.
///
/// For String values: detects expression pattern (`name(...)`) or
/// interpolation (`{expr}`).
/// For Table values: recursively resolves each leaf value.
/// For other values (Int, Float, Bool): passes through unchanged.
pub fn resolve_toml_value(
    value: &toml::Value,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Option<Value>, ConfigError> {
    match value {
        toml::Value::String(s) => resolve_string(s, registry, context),
        toml::Value::Integer(i) => Ok(Some(Value::Int64(*i))),
        toml::Value::Float(f) => Ok(Some(Value::Float64(*f))),
        toml::Value::Boolean(b) => Ok(Some(Value::Bool(*b))),
        toml::Value::Table(table) => resolve_table(table, registry, context),
        toml::Value::Array(arr) => resolve_array(arr, registry, context),
        // Datetime etc. — not supported as expression
        _ => Ok(None),
    }
}

fn resolve_string(
    s: &str,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Option<Value>, ConfigError> {
    if is_expression(s) {
        let value = eval_expression(s, registry, context).map_err(|e| ConfigError::Invalid {
            reason: format!("expression error: {e}"),
        })?;
        Ok(Some(value))
    } else if has_interpolation(s) {
        let result = eval_interpolated(s, registry, context).map_err(|e| ConfigError::Invalid {
            reason: format!("interpolation error: {e}"),
        })?;
        Ok(Some(Value::Text(result)))
    } else {
        Ok(Some(Value::Text(s.to_string())))
    }
}

fn resolve_table(
    table: &toml::map::Map<String, toml::Value>,
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Option<Value>, ConfigError> {
    let mut entries: Vec<(String, Value)> = Vec::with_capacity(table.len());
    for (key, val) in table {
        if let Some(resolved) = resolve_toml_value(val, registry, context)? {
            entries.push((key.clone(), resolved));
        }
    }
    Ok(Some(Value::Object(entries)))
}

fn resolve_array(
    arr: &[toml::Value],
    registry: &FunctionRegistry,
    context: &EvalContext,
) -> Result<Option<Value>, ConfigError> {
    let mut values = Vec::with_capacity(arr.len());
    for val in arr {
        if let Some(resolved) = resolve_toml_value(val, registry, context)? {
            values.push(resolved);
        }
    }
    let json_arr: Vec<serde_json::Value> = values
        .iter()
        .map(|v| air_elt_types::value_to_json(v).unwrap_or(serde_json::Value::Null))
        .collect();
    Ok(Some(Value::Json(serde_json::Value::Array(json_arr))))
}

/// Build an [`EvalContext`] for expression evaluation during config loading.
///
/// Uses the real system environment and filesystem, scoped to `config_dir`
/// for relative file reads.
pub fn build_eval_context(config_dir: &Path) -> EvalContext {
    EvalContext {
        env_resolver: Arc::new(SystemEnvResolver),
        file_resolver: Arc::new(SystemFileResolver),
        now: chrono::Utc::now(),
        base_dir: config_dir.to_path_buf(),
    }
}

struct SystemEnvResolver;

impl EnvResolver for SystemEnvResolver {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

struct SystemFileResolver;

impl FileResolver for SystemFileResolver {
    fn read(&self, path: &str, base_dir: &Path) -> Result<String, FuncError> {
        use air_elt_expr_types::limits::MAX_EXPR_FILE_BYTES;

        if Path::new(path).is_absolute() {
            return Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: "absolute paths not allowed in expressions".to_owned(),
            });
        }

        let resolved = base_dir.join(path);

        let canonical = resolved
            .canonicalize()
            .map_err(|e| FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: e.to_string(),
            })?;

        let base_canonical = base_dir
            .canonicalize()
            .unwrap_or_else(|_| base_dir.to_path_buf());
        if !canonical.starts_with(&base_canonical) {
            return Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: "path traversal not allowed".to_owned(),
            });
        }

        let metadata = std::fs::metadata(&canonical).map_err(|e| FuncError::FileReadFailed {
            path: path.to_owned(),
            reason: e.to_string(),
        })?;

        if metadata.len() > MAX_EXPR_FILE_BYTES as u64 {
            return Err(FuncError::FileReadFailed {
                path: path.to_owned(),
                reason: format!(
                    "file too large: {} bytes (max {})",
                    metadata.len(),
                    MAX_EXPR_FILE_BYTES,
                ),
            });
        }

        std::fs::read_to_string(&canonical).map_err(|e| FuncError::FileReadFailed {
            path: path.to_owned(),
            reason: e.to_string(),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn test_registry() -> FunctionRegistry {
        FunctionRegistry::with_builtins()
    }

    fn test_context() -> EvalContext {
        EvalContext {
            env_resolver: Arc::new(SystemEnvResolver),
            file_resolver: Arc::new(SystemFileResolver),
            now: chrono::Utc::now(),
            base_dir: std::path::PathBuf::from("/tmp"),
        }
    }

    #[test]
    fn plain_string_passes_through() {
        let value = toml::Value::String("hello".to_string());
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, Some(Value::Text("hello".to_string())));
    }

    #[test]
    fn integer_passes_through() {
        let value = toml::Value::Integer(42);
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, Some(Value::Int64(42)));
    }

    #[test]
    fn float_passes_through() {
        let value = toml::Value::Float(2.71);
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, Some(Value::Float64(2.71)));
    }

    #[test]
    fn boolean_passes_through() {
        let value = toml::Value::Boolean(true);
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, Some(Value::Bool(true)));
    }

    #[test]
    fn expression_is_evaluated() {
        let value = toml::Value::String("concat('hello', ' ', 'world')".to_string());
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, Some(Value::Text("hello world".to_string())));
    }

    #[test]
    fn interpolation_is_evaluated() {
        let value = toml::Value::String("prefix_{1 + 2}_suffix".to_string());
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, Some(Value::Text("prefix_3_suffix".to_string())));
    }

    #[test]
    fn table_resolves_recursively() {
        let mut table = toml::map::Map::new();
        table.insert("key".to_string(), toml::Value::String("plain".to_string()));
        table.insert("num".to_string(), toml::Value::Integer(7));
        let value = toml::Value::Table(table);
        let result = resolve_toml_value(&value, &test_registry(), &test_context()).unwrap();
        match result {
            Some(Value::Object(entries)) => {
                assert_eq!(entries.len(), 2);
                assert!(
                    entries
                        .iter()
                        .any(|(k, v)| k == "key" && *v == Value::Text("plain".to_string()))
                );
                assert!(
                    entries
                        .iter()
                        .any(|(k, v)| k == "num" && *v == Value::Int64(7))
                );
            }
            other => panic!("expected Object, got {other:?}"),
        }
    }

    #[test]
    fn invalid_expression_returns_error() {
        let value = toml::Value::String("nonexistent_func(1)".to_string());
        let result = resolve_toml_value(&value, &test_registry(), &test_context());
        assert!(result.is_err());
    }

    #[test]
    fn datetime_returns_none() {
        let dt = toml::Value::Datetime(toml::value::Datetime {
            date: Some(toml::value::Date {
                year: 2024,
                month: 1,
                day: 1,
            }),
            time: None,
            offset: None,
        });
        let result = resolve_toml_value(&dt, &test_registry(), &test_context()).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn array_resolves_elements() {
        let arr = toml::Value::Array(vec![
            toml::Value::Integer(1),
            toml::Value::String("hello".to_string()),
            toml::Value::Boolean(true),
        ]);
        let result = resolve_toml_value(&arr, &test_registry(), &test_context()).unwrap();
        match result {
            Some(Value::Json(serde_json::Value::Array(items))) => {
                assert_eq!(items.len(), 3);
                assert_eq!(items[0], serde_json::json!(1));
                assert_eq!(items[1], serde_json::json!("hello"));
                assert_eq!(items[2], serde_json::json!(true));
            }
            other => panic!("expected Json(Array), got {other:?}"),
        }
    }

    #[test]
    fn build_eval_context_uses_provided_dir() {
        let ctx = build_eval_context(Path::new("/some/dir"));
        assert_eq!(ctx.base_dir, std::path::PathBuf::from("/some/dir"));
    }

    #[test]
    fn file_resolver_rejects_absolute_path() {
        let resolver = SystemFileResolver;
        let result = resolver.read("/etc/passwd", Path::new("/tmp"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        match err {
            FuncError::FileReadFailed { reason, .. } => {
                assert!(reason.contains("absolute paths not allowed"));
            }
            other => panic!("expected FileReadFailed, got {other:?}"),
        }
    }

    #[test]
    fn file_resolver_rejects_path_traversal() {
        let resolver = SystemFileResolver;
        let result = resolver.read("../../etc/passwd", Path::new("/tmp/subdir"));
        // This will either fail with "path traversal" or "No such file"
        // depending on whether the path can be canonicalized.
        assert!(result.is_err());
    }
}
