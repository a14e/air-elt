use serde::{Deserialize, Serialize};

/// Canonical "pivot" type. Each connector maps native ↔ canonical, the runner
/// uses the matrix in `super::matrix` to validate compatibility.
///
/// `Text` and `Bytes` carry an optional declared size (`varchar(36)`,
/// `binary(16)`, etc.). `None` means unbounded (`text`, `mediumtext`, `blob`).
/// The size is part of the *schema*, not the *value* — `Value::Text` stores a
/// plain `String` regardless. Width is consulted only at validation time so
/// the matrix can reject narrowing pairs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Bool,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    Text { size: Option<u32> },
    Bytes { size: Option<u32> },
    Date,
    Timestamp,
    Uuid,
    Json,
}

impl DataType {
    /// Convenience constructor for unbounded text.
    pub const fn text() -> Self {
        DataType::Text { size: None }
    }

    /// Convenience constructor for unbounded bytes.
    pub const fn bytes() -> Self {
        DataType::Bytes { size: None }
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Bool => f.write_str("bool"),
            DataType::Int16 => f.write_str("int16"),
            DataType::Int32 => f.write_str("int32"),
            DataType::Int64 => f.write_str("int64"),
            DataType::Float32 => f.write_str("float32"),
            DataType::Float64 => f.write_str("float64"),
            DataType::Text { size: None } => f.write_str("text"),
            DataType::Text { size: Some(n) } => write!(f, "text({n})"),
            DataType::Bytes { size: None } => f.write_str("bytes"),
            DataType::Bytes { size: Some(n) } => write!(f, "bytes({n})"),
            DataType::Date => f.write_str("date"),
            DataType::Timestamp => f.write_str("timestamp"),
            DataType::Uuid => f.write_str("uuid"),
            DataType::Json => f.write_str("json"),
        }
    }
}
