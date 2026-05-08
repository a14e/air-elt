//! Canonical set of PostgreSQL column types the project recognises, plus the
//! one-way mapping `pg → internal DataType`. The reverse mapping
//! (`internal → pg`) is sink-specific and lives in `sinks/postgres`.
//!
//! Why here and not per-connector: the native-type grammar is a property of
//! the dialect, not of the direction of data flow. Keeping one authoritative
//! table avoids drift between source and sink when a new type is added.

use air_elt_core::types::DataType;

use super::types::PgHllType;

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
    /// `numeric` / `decimal`. Precision and scale come from a separate
    /// information_schema column — see `to_internal`.
    Numeric,
    /// PG `xml` — surfaces over the wire as text but is its own canonical
    /// `DataType` so the matrix can apply XML-specific rules (well-formed
    /// validation, forbidden Xml→Xml truncation).
    Xml,
    /// `hll` from the `postgresql-hll` extension (bundled in
    /// `citusdata/citus`). User-defined extension types arrive in
    /// `information_schema` as `data_type = 'USER-DEFINED'`,
    /// `udt_name = '<extname>'` — we identify by `udt_name`.
    /// Mapped to a connector-defined `Custom(PgHllType)` rather than a
    /// canonical `DataType` variant; HLL has no canonical analogue.
    Hll,
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
            "numeric" | "decimal" => PgType::Numeric,
            "xml" => PgType::Xml,
            // `hll` only appears as a `udt_name`; matching it here as well so
            // a caller passing the `udt_name` string also resolves it (the
            // `data_type` column is `'USER-DEFINED'` for extension types
            // and never equals `"hll"`).
            "hll" => PgType::Hll,
            // `timestamp` / `timestamp without time zone` intentionally omitted.
            _ => return None,
        };
        Some(result)
    }
}

/// Map a PG type to the canonical `DataType`.
///
/// `char_max_length` comes from `information_schema.columns` and is folded
/// into `Text`/`Bytes` size. For unbounded `text` / `bytea` it is `None`. For
/// non-text/bytes types the parameter is ignored.
///
/// `numeric_precision` / `numeric_scale` are read for `numeric`/`decimal`
/// columns. PG returns both as NULL when the column was declared without a
/// modifier (`numeric`) — the column can then carry any precision and scale
/// at runtime, so we surface it as fully-unbounded `Decimal`.
pub fn to_internal(
    pg: PgType,
    char_max_length: Option<u32>,
    numeric_precision: Option<u32>,
    numeric_scale: Option<u32>,
) -> DataType {
    match pg {
        PgType::Bool => DataType::Bool,
        PgType::Int2 => DataType::Int16,
        PgType::Int4 => DataType::Int32,
        PgType::Int8 => DataType::Int64,
        PgType::Float4 => DataType::Float32,
        PgType::Float8 => DataType::Float64,
        // `text` is unbounded by default; `varchar`/`bpchar` may carry a size.
        PgType::Text => DataType::Text { size: None },
        PgType::Varchar | PgType::Bpchar => DataType::Text {
            size: char_max_length,
        },
        PgType::Bytea => DataType::Bytes { size: None },
        PgType::Date => DataType::Date,
        PgType::TimestampTz => DataType::Timestamp,
        PgType::Uuid => DataType::Uuid,
        PgType::Json | PgType::Jsonb => DataType::Json,
        PgType::Numeric => match (numeric_precision, numeric_scale) {
            // numeric(p, 0) → BigInt with declared digit-width.
            (Some(p), Some(0)) => DataType::BigInt { width: Some(p) },
            // numeric(p, s) with s > 0 → fractional decimal.
            (Some(p), Some(s)) => DataType::Decimal {
                precision: Some(p),
                scale: Some(s),
            },
            // `numeric` without modifier — fully unbounded.
            (None, _) => DataType::Decimal {
                precision: None,
                scale: None,
            },
            // Precision without scale is non-canonical in PG; treat as scale 0.
            (Some(p), None) => DataType::BigInt { width: Some(p) },
        },
        PgType::Xml => DataType::Xml,
        PgType::Hll => DataType::Custom(Box::new(PgHllType)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
    fn hll_parses_from_udt_name() {
        assert_eq!(PgType::parse("hll"), Some(PgType::Hll));
        assert_eq!(PgType::parse("HLL"), Some(PgType::Hll));
    }

    #[test]
    fn hll_maps_to_custom_data_type() {
        let dt = to_internal(PgType::Hll, None, None, None);
        match dt {
            DataType::Custom(t) => assert_eq!(t.kind(), "postgresql.hll"),
            other => panic!("expected DataType::Custom(hll), got {other:?}"),
        }
    }

    #[test]
    fn numeric_zero_scale_is_bigint() {
        assert_eq!(
            to_internal(PgType::Numeric, None, Some(20), Some(0)),
            DataType::BigInt { width: Some(20) }
        );
    }

    #[test]
    fn numeric_with_scale_is_decimal() {
        assert_eq!(
            to_internal(PgType::Numeric, None, Some(10), Some(2)),
            DataType::Decimal {
                precision: Some(10),
                scale: Some(2)
            }
        );
    }

    #[test]
    fn unbounded_numeric_is_unbounded_decimal() {
        assert_eq!(
            to_internal(PgType::Numeric, None, None, None),
            DataType::Decimal {
                precision: None,
                scale: None
            }
        );
    }

    #[test]
    fn internal_mapping_sample() {
        assert_eq!(to_internal(PgType::Int8, None, None, None), DataType::Int64);
        assert_eq!(to_internal(PgType::Jsonb, None, None, None), DataType::Json);
        assert_eq!(
            to_internal(PgType::Text, None, None, None),
            DataType::Text { size: None }
        );
        assert_eq!(
            to_internal(PgType::Bpchar, Some(36), None, None),
            DataType::Text { size: Some(36) }
        );
        assert_eq!(
            to_internal(PgType::Varchar, Some(255), None, None),
            DataType::Text { size: Some(255) }
        );
        assert_eq!(
            to_internal(PgType::Bytea, None, None, None),
            DataType::Bytes { size: None }
        );
    }
}
