use std::path::Path;
use std::sync::Arc;

use air_elt_expr_funcs::signature::{EnvResolver, EvalContext, FileResolver};
use air_elt_expr_funcs::{FuncError, FunctionRegistry};
use air_elt_expr_optimize::{ExpectedOutput, Optimizer};
use air_elt_expr_parse::{Parser, Program};
use air_elt_types::{DataType, Schema, Value};

use crate::error::ExprError;
use crate::program::RuntimeProgram;

#[derive(Clone)]
pub struct ExpressionContext {
    pub(crate) registry: Arc<FunctionRegistry>,
    pub(crate) eval_context: EvalContext,
}

impl ExpressionContext {
    pub fn create(registry: Arc<FunctionRegistry>, config_dir: &Path) -> Self {
        Self {
            registry,
            eval_context: build_eval_context(config_dir),
        }
    }

    /// Compile a field-free (comptime) program through the optimizer and
    /// evaluate it on the arena evaluator. This is the single production path
    /// for const / default / switch literals and config patching: the optimizer
    /// folds the pure parts to a constant and the arena evaluator runs whatever
    /// remains (impure `env`/`file`/`now` calls), exactly as the program means.
    pub fn evaluate_const(&self, program: &Program) -> Result<Value, ExprError> {
        // Comptime const evaluation has no source schema and no sink column, so
        // the type pass is skipped (None, None) — the program's surviving
        // `TypeAssert`s do any per-row type checking.
        let compact =
            Optimizer::create(&self.registry, &self.eval_context).compile(program, None, None)?;
        let value = RuntimeProgram::create(compact).evaluate(&self.registry, &self.eval_context)?;
        Ok(value)
    }

    /// Compile a per-row compute script (runtime grammar: `field`/`fields`/
    /// backtick allowed) into a [`RuntimeProgram`]. This is the single
    /// production entry point core uses to compile a `compute = "<expr>"`
    /// column — it keeps core off `expr-optimize` entirely.
    ///
    /// `schema` is the source-side schema used by the typed pass to check
    /// field existence (a field absent from a *fixed* schema is an error)
    /// and infer types; pass `None` for schemaless sources. `expected`,
    /// when set, is the sink column's `(DataType, truncate)` pair — the
    /// optimizer validates the program's result type against it, surfacing
    /// an output-type mismatch at compile time instead of at the per-row
    /// `Convert`.
    pub fn compile_runtime(
        &self,
        source: &str,
        schema: Option<&Schema>,
        expected: Option<(&DataType, bool)>,
    ) -> Result<RuntimeProgram, ExprError> {
        let program = Parser::create().parse_expression(source)?;
        let expected_output = expected.map(|(data_type, truncate)| ExpectedOutput {
            data_type: data_type.clone(),
            truncate,
        });
        let compact = Optimizer::create(&self.registry, &self.eval_context).compile(
            &program,
            schema,
            expected_output.as_ref(),
        )?;
        Ok(RuntimeProgram::create(compact))
    }

    /// The shared function registry — the runner needs it to evaluate a
    /// compiled compute program per row.
    pub fn registry(&self) -> &Arc<FunctionRegistry> {
        &self.registry
    }

    /// The base per-flow evaluation context. The runner clones it with a
    /// fresh batch clock via [`EvalContext::with_now`] each tick.
    pub fn eval_context(&self) -> &EvalContext {
        &self.eval_context
    }
}

fn build_eval_context(config_dir: &Path) -> EvalContext {
    EvalContext {
        env_resolver: Arc::new(SystemEnvResolver),
        file_resolver: Arc::new(SystemFileResolver),
        now: chrono::Utc::now(),
        base_dir: config_dir.to_path_buf(),
        is_compile_time: false,
        caches: air_elt_expr_funcs::ExprCaches::default(),
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

    #[test]
    fn create_uses_provided_dir() {
        let ctx = ExpressionContext::create(
            Arc::new(FunctionRegistry::with_builtins()),
            Path::new("/some/dir"),
        );
        assert_eq!(
            ctx.eval_context.base_dir,
            std::path::PathBuf::from("/some/dir")
        );
    }

    #[test]
    fn compile_runtime_compiles_and_evaluates_a_row() {
        use air_elt_types::{DataType, Field, Schema, Value};

        let ctx = ExpressionContext::create(
            Arc::new(FunctionRegistry::with_builtins()),
            Path::new("/tmp"),
        );
        let schema = Schema::new(vec![
            Field {
                name: "a".to_owned(),
                data_type: DataType::Text { size: None },
                nullable: false,
            },
            Field {
                name: "b".to_owned(),
                data_type: DataType::Text { size: None },
                nullable: false,
            },
        ]);
        let program = ctx
            .compile_runtime(
                "concat(`a`, `b`)",
                Some(&schema),
                Some((&DataType::Text { size: None }, false)),
            )
            .expect("compiles against the schema");
        assert_eq!(program.needed_columns(), &["a".to_string(), "b".into()]);
        let value = program
            .evaluate_row(
                ctx.registry(),
                ctx.eval_context(),
                &schema,
                &[Value::Text("x".into()), Value::Text("y".into())],
            )
            .expect("evaluates");
        assert_eq!(value, Value::Text("xy".into()));
    }

    #[test]
    fn compile_runtime_rejects_field_absent_from_fixed_schema() {
        use air_elt_types::{DataType, Field, Schema};

        let ctx = ExpressionContext::create(
            Arc::new(FunctionRegistry::with_builtins()),
            Path::new("/tmp"),
        );
        let schema = Schema::new(vec![Field {
            name: "a".to_owned(),
            data_type: DataType::Int64,
            nullable: false,
        }]);
        let err = ctx
            .compile_runtime(
                "`missing` + 1",
                Some(&schema),
                Some((&DataType::Int64, false)),
            )
            .expect_err("absent field in a fixed schema is a compile error");
        // Surfaces through the optimizer's type pass.
        assert!(matches!(err, ExprError::Optimize(_)));
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
}
