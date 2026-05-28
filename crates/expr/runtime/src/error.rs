use air_elt_expr_funcs::FuncError;
use air_elt_expr_parse::ExprError as ParseError;
use thiserror::Error;

/// Unified expression error covering parsing, type-checking, and evaluation.
#[derive(Debug, Error)]
pub enum ExprError {
    #[error(transparent)]
    Parse(#[from] ParseError),

    #[error("undefined variable: {name}")]
    UndefinedVariable { name: String },

    #[error("type error: {0}")]
    Type(#[from] air_elt_expr_types::error::ExprTypeError),

    #[error("function error: {0}")]
    Function(#[from] FuncError),

    #[error("invalid patcher pattern: {0}")]
    InvalidPattern(String),
}
