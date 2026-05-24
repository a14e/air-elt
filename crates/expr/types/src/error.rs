use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExprTypeError {
    #[error("integer overflow: result exceeds {max} bits")]
    IntegerOverflow { max: u32 },

    #[error("string too large: {len} bytes exceeds maximum {max}")]
    StringTooLarge { len: usize, max: usize },

    #[error("type mismatch: cannot apply {operation} to {left} and {right}")]
    TypeMismatch {
        operation: String,
        left: String,
        right: String,
    },

    #[error("unsupported conversion: {from} to {to}")]
    UnsupportedConversion { from: String, to: String },
}
