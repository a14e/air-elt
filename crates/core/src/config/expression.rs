use std::path::Path;
use std::sync::Arc;

use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
use air_elt_expr_funcs::{FuncError, FunctionRegistry};

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

pub fn build_eval_context(config_dir: &Path) -> EvalContext {
    EvalContext {
        env_resolver: Arc::new(SystemEnvResolver),
        file_resolver: Arc::new(SystemFileResolver),
        now: chrono::Utc::now(),
        base_dir: config_dir.to_path_buf(),
    }
}

/// Walk a TOML table recursively and evaluate expression strings in place.
/// Non-string values and plain string literals are left untouched; only
/// strings detected as expressions or interpolations by `ExprValue::from_toml`
/// are evaluated. The result is always coerced back to a TOML string (since
/// component config values like `url` are strings anyway).
pub fn resolve_config_expressions(
    table: &toml::Table,
    context: &ExpressionContext,
) -> Result<toml::Table, air_elt_expr::ExprError> {
    let mut resolved = toml::Table::new();
    for (key, value) in table {
        resolved.insert(key.clone(), resolve_toml_value(value, context)?);
    }
    Ok(resolved)
}

fn resolve_toml_value(
    value: &toml::Value,
    context: &ExpressionContext,
) -> Result<toml::Value, air_elt_expr::ExprError> {
    match value {
        toml::Value::String(s) => {
            let expr_val = air_elt_expr::ExprValue::parse(s);
            if !expr_val.needs_eval() {
                return Ok(value.clone());
            }
            let result = expr_val.eval(&context.registry, &context.eval_context)?;
            Ok(value_to_toml(&result))
        }
        toml::Value::Table(t) => {
            let mut out = toml::Table::new();
            for (k, v) in t {
                out.insert(k.clone(), resolve_toml_value(v, context)?);
            }
            Ok(toml::Value::Table(out))
        }
        toml::Value::Array(arr) => {
            let mut out = Vec::with_capacity(arr.len());
            for v in arr {
                out.push(resolve_toml_value(v, context)?);
            }
            Ok(toml::Value::Array(out))
        }
        _ => Ok(value.clone()),
    }
}

/// Convert a runtime `Value` into a `toml::Value`. Used for config resolution
/// (expression results back to TOML) and default-literal coercion.
/// Types that don't map to a native TOML scalar fall back to their string
/// representation.
pub(crate) fn value_to_toml(value: &air_elt_types::Value) -> toml::Value {
    use air_elt_types::Value;
    match value {
        Value::Text(s) => toml::Value::String(s.clone()),
        Value::Int64(n) => toml::Value::Integer(*n),
        Value::Float64(f) => toml::Value::Float(*f),
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Int8(n) => toml::Value::Integer(*n as i64),
        Value::Int16(n) => toml::Value::Integer(*n as i64),
        Value::Int32(n) => toml::Value::Integer(*n as i64),
        Value::UInt8(n) => toml::Value::Integer(*n as i64),
        Value::UInt16(n) => toml::Value::Integer(*n as i64),
        Value::UInt32(n) => toml::Value::Integer(*n as i64),
        Value::UInt64(n) => i64::try_from(*n)
            .map(toml::Value::Integer)
            .unwrap_or_else(|_| toml::Value::String(n.to_string())),
        Value::Float32(f) => toml::Value::Float(*f as f64),
        Value::BigInt(b) => toml::Value::String(b.to_string()),
        Value::Decimal(d) => toml::Value::String(d.to_string()),
        Value::Date(d) => toml::Value::String(d.to_string()),
        Value::Timestamp(ts) => toml::Value::String(ts.to_rfc3339()),
        Value::Uuid(u) => toml::Value::String(u.to_string()),
        Value::Ipv4(ip) => toml::Value::String(ip.to_string()),
        Value::Ipv6(ip) => toml::Value::String(ip.to_string()),
        _ => toml::Value::String(format!("{value:?}")),
    }
}

/// Verify the evaluated Value matches the sink DataType; if not, attempt
/// value-aware narrowing (int/float fit check) or lossless widening via
/// `air_elt_types::convert`. Errors when the value cannot be represented
/// in the target type.
pub(crate) fn ensure_sink_compatible(
    value: air_elt_types::Value,
    sink_dt: &air_elt_types::DataType,
) -> Result<air_elt_types::Value, String> {
    use air_elt_types::{DataType, Value};

    if let Some(ref value_dt) = value.data_type() {
        if value_dt == sink_dt {
            return Ok(value);
        }
        // Text/Bytes: check actual length against the sink's declared size.
        match (&value, sink_dt) {
            (Value::Text(s), DataType::Text { size: Some(max) }) => {
                let chars = s.chars().count();
                if chars > *max as usize {
                    return Err(format!("text length {chars} exceeds sink size {max}"));
                }
                return Ok(value);
            }
            (Value::Bytes(b), DataType::Bytes { size: Some(max) }) => {
                if b.len() > *max as usize {
                    return Err(format!("bytes length {} exceeds sink size {max}", b.len()));
                }
                return Ok(value);
            }
            _ => {}
        }
        // Numeric narrowing: TOML gives us Int64/Float64, but the sink may
        // be a narrower type. Check the actual value fits, then cast.
        if let Some(narrowed) = try_narrow_numeric(&value, sink_dt) {
            return narrowed;
        }
        let ctx = air_elt_types::ConversionContext::passthrough();
        return air_elt_types::convert(value, value_dt, sink_dt, &ctx)
            .map_err(|e| format!("cannot convert {value_dt} to {sink_dt}: {e}"));
    }
    Ok(value)
}

/// Try to narrow a numeric Value to the target DataType by checking the
/// actual value fits. Returns `None` if this is not a numeric narrowing
/// case — the caller should fall through to `convert()`.
fn try_narrow_numeric(
    value: &air_elt_types::Value,
    target: &air_elt_types::DataType,
) -> Option<Result<air_elt_types::Value, String>> {
    use air_elt_types::{DataType, Value};

    let n = match value {
        Value::Int64(n) => *n,
        Value::Float64(f) => {
            return match target {
                DataType::Float32 => Some(Ok(Value::Float32(*f as f32))),
                _ => None,
            };
        }
        _ => return None,
    };

    let result = match target {
        DataType::Int8 if (i8::MIN as i64..=i8::MAX as i64).contains(&n) => Value::Int8(n as i8),
        DataType::Int16 if (i16::MIN as i64..=i16::MAX as i64).contains(&n) => {
            Value::Int16(n as i16)
        }
        DataType::Int32 if (i32::MIN as i64..=i32::MAX as i64).contains(&n) => {
            Value::Int32(n as i32)
        }
        DataType::UInt8 if (0..=u8::MAX as i64).contains(&n) => Value::UInt8(n as u8),
        DataType::UInt16 if (0..=u16::MAX as i64).contains(&n) => Value::UInt16(n as u16),
        DataType::UInt32 if (0..=u32::MAX as i64).contains(&n) => Value::UInt32(n as u32),
        DataType::UInt64 if n >= 0 => Value::UInt64(n as u64),
        DataType::Float32 => Value::Float32(n as f32),
        DataType::Float64 => Value::Float64(n as f64),
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => {
            return Some(Err(format!("value {n} out of range for {target}")));
        }
        _ => return None,
    };
    Some(Ok(result))
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
        assert!(result.is_err());
    }

    fn test_expr_context() -> ExpressionContext {
        ExpressionContext::new(
            Arc::new(FunctionRegistry::with_builtins()),
            Path::new("/tmp"),
        )
    }

    #[test]
    fn resolve_config_expressions_evaluates_env_call() {
        #[allow(unsafe_code)]
        // Why: setting env var needed to test env() expression resolution
        unsafe {
            std::env::set_var("AIR_ELT_TEST_CFG_URL", "postgres://localhost/test");
        }
        let mut table = toml::Table::new();
        table.insert(
            "url".into(),
            toml::Value::String("env('AIR_ELT_TEST_CFG_URL')".into()),
        );

        let ctx = test_expr_context();
        let resolved = resolve_config_expressions(&table, &ctx).unwrap();
        assert_eq!(
            resolved.get("url").unwrap().as_str().unwrap(),
            "postgres://localhost/test"
        );

        #[allow(unsafe_code)]
        // Why: cleanup
        unsafe {
            std::env::remove_var("AIR_ELT_TEST_CFG_URL");
        }
    }

    #[test]
    fn resolve_config_expressions_leaves_plain_strings_intact() {
        let mut table = toml::Table::new();
        table.insert(
            "url".into(),
            toml::Value::String("postgres://localhost/db".into()),
        );
        table.insert("port".into(), toml::Value::Integer(5432));

        let ctx = test_expr_context();
        let resolved = resolve_config_expressions(&table, &ctx).unwrap();
        assert_eq!(
            resolved.get("url").unwrap().as_str().unwrap(),
            "postgres://localhost/db"
        );
        assert_eq!(resolved.get("port").unwrap().as_integer().unwrap(), 5432);
    }

    #[test]
    fn resolve_config_expressions_handles_nested_tables() {
        let mut inner = toml::Table::new();
        inner.insert(
            "value".into(),
            toml::Value::String("concat('hello', ' world')".into()),
        );
        let mut table = toml::Table::new();
        table.insert("nested".into(), toml::Value::Table(inner));

        let ctx = test_expr_context();
        let resolved = resolve_config_expressions(&table, &ctx).unwrap();
        let nested = resolved.get("nested").unwrap().as_table().unwrap();
        assert_eq!(
            nested.get("value").unwrap().as_str().unwrap(),
            "hello world"
        );
    }

    #[test]
    fn resolve_config_expressions_interpolation() {
        let mut table = toml::Table::new();
        table.insert(
            "url".into(),
            toml::Value::String("prefix_{1 + 2}_suffix".into()),
        );

        let ctx = test_expr_context();
        let resolved = resolve_config_expressions(&table, &ctx).unwrap();
        assert_eq!(
            resolved.get("url").unwrap().as_str().unwrap(),
            "prefix_3_suffix"
        );
    }

    #[test]
    fn resolve_config_expressions_handles_arrays() {
        let mut table = toml::Table::new();
        table.insert(
            "tags".into(),
            toml::Value::Array(vec![
                toml::Value::String("concat('a', 'b')".into()),
                toml::Value::String("plain".into()),
            ]),
        );

        let ctx = test_expr_context();
        let resolved = resolve_config_expressions(&table, &ctx).unwrap();
        let arr = resolved.get("tags").unwrap().as_array().unwrap();
        assert_eq!(arr[0].as_str().unwrap(), "ab");
        assert_eq!(arr[1].as_str().unwrap(), "plain");
    }

    #[test]
    fn resolve_config_expressions_invalid_expression_propagates_error() {
        let mut table = toml::Table::new();
        table.insert(
            "url".into(),
            toml::Value::String("nonexistent_func()".into()),
        );

        let ctx = test_expr_context();
        let result = resolve_config_expressions(&table, &ctx);
        assert!(result.is_err());
    }
}
