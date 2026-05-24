use thiserror::Error;

#[derive(Debug, Error)]
pub enum FuncError {
    #[error("type mismatch in {function}: expected {expected}, got {actual}")]
    TypeMismatch {
        function: String,
        expected: String,
        actual: String,
    },

    #[error("wrong number of arguments for {function}: expected {expected}, got {actual}")]
    ArityMismatch {
        function: String,
        expected: String,
        actual: usize,
    },

    #[error("evaluation failed in {function}: {reason}")]
    EvalFailed { function: String, reason: String },

    #[error("unknown function: {name}")]
    UnknownFunction { name: String },

    #[error("ambiguous overload for {function} with argument types: {arg_types}")]
    AmbiguousOverload { function: String, arg_types: String },

    #[error("null argument not allowed for {function} at position {position}")]
    NullNotAllowed { function: String, position: usize },

    #[error("division by zero")]
    DivisionByZero,

    #[error("integer overflow")]
    IntegerOverflow,

    #[error("string too large: {len} bytes (max {max})")]
    StringTooLarge { len: usize, max: usize },

    #[error("regex compilation failed: {reason}")]
    RegexCompileFailed { reason: String },

    #[error("file read failed: {path}: {reason}")]
    FileReadFailed { path: String, reason: String },

    #[error("encoding error: {reason}")]
    EncodingError { reason: String },

    #[error("json path error: {reason}")]
    JsonPathError { reason: String },

    #[error("invalid argument in {function}: {message}")]
    InvalidArgument { function: String, message: String },
}
