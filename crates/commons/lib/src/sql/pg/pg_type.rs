//! Canonical set of PostgreSQL column types the project recognises, plus the
//! one-way mapping `pg → internal DataType`. The reverse mapping
//! (`internal → pg`) is sink-specific and lives in `sinks/postgres`.
//!
//! Why here and not per-connector: the native-type grammar is a property of
//! the dialect, not of the direction of data flow. Keeping one authoritative
//! table avoids drift between source and sink when a new type is added.

use air_elt_core::error::TypeError;
use air_elt_core::types::DataType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PgType {
    Bool,
    Int2,
    Int4,
    Int8,
    Float4,
    Float8,
    Text,
    Varchar,
    Bpchar,
    Bytea,
    Date,
    TimestampTz,
    Uuid,
    Json,
    Jsonb,
}

impl PgType {
    /// Parse a `udt_name` or `data_type` string from `information_schema.columns`.
    ///
    /// Why `timestamp` (without TZ) returns `None`: MVP refuses naive
    /// timestamps — they silently shift when the session `TimeZone` GUC
    /// changes. Operators must migrate to `timestamptz`. Explicit tz-aware
    /// transforms can be reintroduced once the `transform` feature lands.
    pub fn parse(name: &str) -> Option<Self> {
        let norm = name.trim().to_ascii_lowercase();
        let result = match norm.as_str() {
            "bool" | "boolean" => PgType::Bool,
            "int2" | "smallint" => PgType::Int2,
            "int4" | "integer" | "int" => PgType::Int4,
            "int8" | "bigint" => PgType::Int8,
            "float4" | "real" => PgType::Float4,
            "float8" | "double precision" => PgType::Float8,
            "text" => PgType::Text,
            "varchar" | "character varying" => PgType::Varchar,
            "bpchar" | "character" | "char" => PgType::Bpchar,
            "bytea" => PgType::Bytea,
            "date" => PgType::Date,
            "timestamptz" | "timestamp with time zone" => PgType::TimestampTz,
            "uuid" => PgType::Uuid,
            "json" => PgType::Json,
            "jsonb" => PgType::Jsonb,
            // `timestamp` / `timestamp without time zone` intentionally omitted.
            _ => return None,
        };
        Some(result)
    }
}

pub fn parse_or_err(name: &str) -> Result<PgType, TypeError> {
    PgType::parse(name).ok_or_else(|| TypeError::UnsupportedNativeType {
        native: name.to_string(),
    })
}

pub fn to_internal(pg: PgType) -> DataType {
    match pg {
        PgType::Bool => DataType::Bool,
        PgType::Int2 => DataType::Int16,
        PgType::Int4 => DataType::Int32,
        PgType::Int8 => DataType::Int64,
        PgType::Float4 => DataType::Float32,
        PgType::Float8 => DataType::Float64,
        PgType::Text | PgType::Varchar | PgType::Bpchar => DataType::Text,
        PgType::Bytea => DataType::Bytes,
        PgType::Date => DataType::Date,
        PgType::TimestampTz => DataType::Timestamp,
        PgType::Uuid => DataType::Uuid,
        PgType::Json | PgType::Jsonb => DataType::Json,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_parse() {
        assert_eq!(PgType::parse("int4"), Some(PgType::Int4));
        assert_eq!(PgType::parse("integer"), Some(PgType::Int4));
        assert_eq!(
            PgType::parse("timestamp with time zone"),
            Some(PgType::TimestampTz)
        );
        assert_eq!(PgType::parse("TIMESTAMPTZ"), Some(PgType::TimestampTz));
    }

    #[test]
    fn naive_timestamp_rejected() {
        // see docstring on PgType::parse
        assert!(PgType::parse("timestamp").is_none());
        assert!(PgType::parse("timestamp without time zone").is_none());
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(PgType::parse("money").is_none());
    }

    #[test]
    fn internal_mapping_sample() {
        assert_eq!(to_internal(PgType::Int8), DataType::Int64);
        assert_eq!(to_internal(PgType::Jsonb), DataType::Json);
        assert_eq!(to_internal(PgType::Bpchar), DataType::Text);
    }
}
