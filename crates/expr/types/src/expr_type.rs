use air_elt_types::DataType;

use crate::nullable::NullableExprType;

/// Expression-level type. Wraps the canonical DataType with bounds tracking.
/// During expression evaluation, arithmetic on bounded types (Text{size}, BigInt{width})
/// produces new bounds. The final ExprType collapses to the nearest canonical DataType.
#[derive(Debug, Clone, PartialEq)]
pub enum ExprType {
    /// Wraps a canonical DataType.
    Scalar(DataType),
    /// Object literal with known keys and typed values.
    Object(Vec<(String, NullableExprType)>),
}

impl ExprType {
    pub fn text(size: Option<u32>) -> Self {
        Self::Scalar(DataType::Text { size })
    }

    pub fn int64() -> Self {
        Self::Scalar(DataType::Int64)
    }

    pub fn float64() -> Self {
        Self::Scalar(DataType::Float64)
    }

    pub fn bool() -> Self {
        Self::Scalar(DataType::Bool)
    }

    pub fn timestamp() -> Self {
        Self::Scalar(DataType::Timestamp)
    }

    pub fn date() -> Self {
        Self::Scalar(DataType::Date)
    }

    pub fn json() -> Self {
        Self::Scalar(DataType::Json)
    }

    pub fn bytes(size: Option<u32>) -> Self {
        Self::Scalar(DataType::Bytes { size })
    }

    pub fn uuid() -> Self {
        Self::Scalar(DataType::Uuid)
    }

    pub fn bigint(width: Option<u32>) -> Self {
        Self::Scalar(DataType::BigInt { width })
    }

    pub fn decimal(precision: Option<u32>, scale: Option<u32>) -> Self {
        Self::Scalar(DataType::Decimal { precision, scale })
    }

    pub fn null_type() -> Self {
        // null resolves to target type at eval time
        Self::Scalar(DataType::Bool)
    }

    pub fn display_name(&self) -> String {
        match self {
            Self::Scalar(dt) => format!("{dt}"),
            Self::Object(_) => "Object".to_owned(),
        }
    }
}

impl From<DataType> for ExprType {
    fn from(dt: DataType) -> Self {
        Self::Scalar(dt)
    }
}
