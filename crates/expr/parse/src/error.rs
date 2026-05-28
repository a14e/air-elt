use thiserror::Error;

/// Expression parse errors.
#[derive(Debug, Error)]
pub enum ExprError {
    #[error("parse error at position {position}: {message}")]
    Parse { position: usize, message: String },

    #[error("unterminated string starting at position {position}")]
    UnterminatedString { position: usize },

    #[error("unterminated interpolation starting at position {position}")]
    UnterminatedInterpolation { position: usize },

    #[error("expression nesting too deep (max {max})")]
    NestingTooDeep { max: usize },

    #[error("expression too long: {len} bytes (max {max})")]
    ExpressionTooLong { len: usize, max: usize },

    #[error("too many AST nodes: {count} (max {max})")]
    TooManyNodes { count: usize, max: usize },

    #[error("too many variables: {count} (max {max})")]
    TooManyVariables { count: usize, max: usize },
}
