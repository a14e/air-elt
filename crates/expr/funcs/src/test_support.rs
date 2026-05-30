use std::path::PathBuf;
use std::sync::Arc;

use crate::error::FuncError;
use crate::signature::{EnvResolver, EvalContext, FileResolver};

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
