use crate::types::DataType;

#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("conversion expected {expected} bytes, got {got}")]
    Length { expected: usize, got: usize },

    #[error("invalid hex digit in input")]
    InvalidHex,

    #[error("input does not parse as a UUID: {reason}")]
    InvalidUuid { reason: String },

    #[error("conversion {src} → {dst} is not supported by the runner")]
    Unsupported { src: DataType, dst: DataType },

    #[error("source value variant does not match declared source DataType {src}")]
    ValueShapeMismatch { src: DataType },
}
