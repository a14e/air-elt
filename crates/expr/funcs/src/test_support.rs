use std::path::PathBuf;
use std::sync::Arc;

use air_elt_types::Value;

use crate::error::FuncError;
use crate::signature::{
    EnvResolver, EvalContext, ExprFunction, FileResolver, FuncArgVec, OwnedArgWindow,
};

pub struct EmptyEnv;

impl EnvResolver for EmptyEnv {
    fn get(&self, _key: &str) -> Option<String> {
        None
    }
}

pub struct NoopFiles;

impl FileResolver for NoopFiles {
    fn read(&self, path: &str, _base_dir: &std::path::Path) -> Result<String, FuncError> {
        Err(FuncError::FileReadFailed {
            path: path.to_owned(),
            reason: "not implemented".to_owned(),
        })
    }
}

pub fn ctx() -> EvalContext {
    EvalContext {
        env_resolver: Arc::new(EmptyEnv),
        file_resolver: Arc::new(NoopFiles),
        now: chrono::Utc::now(),
        base_dir: PathBuf::new(),
        is_compile_time: false,
        caches: crate::cache::ExprCaches::default(),
    }
}

/// Evaluate `func` over owned `values` in a unit test. Wraps the values into an
/// [`OwnedArgWindow`] so test call sites stay terse now that
/// [`ExprFunction::evaluate`] takes a `&mut dyn ArgWindow`.
pub fn eval(
    func: &dyn ExprFunction,
    values: impl Into<FuncArgVec>,
    context: &EvalContext,
) -> Result<Value, FuncError> {
    func.evaluate(&mut OwnedArgWindow::create(values), context)
}
