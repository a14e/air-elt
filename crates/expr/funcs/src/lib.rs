pub mod builtins;
pub mod error;
pub mod registry;
pub mod signature;

pub use error::FuncError;
pub use registry::{FuncRef, FunctionRegistry};
pub use signature::ExprFunction;

impl FunctionRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::new();
        builtins::register_builtins(&mut registry);
        registry
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
}
