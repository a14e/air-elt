use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Null,
    Bool,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    Text,
    Bytes,
    Date,
    Timestamp,
    Uuid,
    Json,
}

impl DataType {
    pub fn name(self) -> &'static str {
        match self {
            DataType::Null => "null",
            DataType::Bool => "bool",
            DataType::Int16 => "int16",
            DataType::Int32 => "int32",
            DataType::Int64 => "int64",
            DataType::Float32 => "float32",
            DataType::Float64 => "float64",
            DataType::Text => "text",
            DataType::Bytes => "bytes",
            DataType::Date => "date",
            DataType::Timestamp => "timestamp",
            DataType::Uuid => "uuid",
            DataType::Json => "json",
        }
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}
