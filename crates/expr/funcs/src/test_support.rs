use crate::error::FuncError;
use crate::signature::{EnvResolver, FileResolver};

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
