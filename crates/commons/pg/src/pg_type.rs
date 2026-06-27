//! Canonical set of PostgreSQL column types the project recognises, plus the
//! one-way mapping `pg → internal DataType`. The reverse mapping
//! (`internal → pg`) is sink-specific and lives in `sinks/postgres`.
//!
//! Why here and not per-connector: the native-type grammar is a property of
//! the dialect, not of the direction of data flow. Keeping one authoritative
//! table avoids drift between source and sink when a new type is added.

use air_elt_core::types::DataType;

use super::types::{PgHllType, PgInetType};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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
    /// PG `inet` — IP host address with optional netmask. Mapped to a
    /// connector-defined `Custom(PgInetType)` so the netmask survives
    /// the pipeline; conversions to canonical `Ipv4` / `Ipv6` drop the
    /// mask under operator `truncate=true` opt-in.
    Inet,
    /// Native PG array of a primitive element (`int4[]`, `text[]`,
    /// `timestamptz[]`, …). PG reports the column either as `T[]`
    /// (`data_type`) or as the internal `_T` name (`udt_name`, e.g.
    /// `_int4`). Restricted to primitive, bindable element types — a
    /// nested array (`int4[][]` / `__int4`) or a non-primitive element
    /// resolves to `None` (unsupported) so it falls through to the
    /// connector's unsupported-type error rather than producing an
    /// `Array(Array(..))` we cannot bind. Maps to canonical
    /// [`DataType::Array`].
    Array(Box<PgType>),
}

impl PgType {
    /// Parse a `udt_name` or `data_type` string from `information_schema.columns`.
    ///
    /// Why `timestamp` (without TZ) returns `None`: MVP refuses naive
    /// timestamps — they silently shift when the session `TimeZone` GUC
    /// changes. Operators must migrate to `timestamptz`. Explicit tz-aware
    /// transforms can be reintroduced once the `transform` feature lands.
    ///
    /// Native array columns are recognised in both forms PG introspection
    /// produces: the `T[]` shape from the `data_type` column and the
    /// internal `_T` shape from the `udt_name` column. The element type is
    /// resolved through the same scalar table and must be a primitive we
    /// can bind — nested arrays and non-primitive elements are rejected.
    pub fn parse(name: &str) -> Option<Self> {
        let norm = name.trim().to_ascii_lowercase();
        if let Some(element_name) = array_element_name(&norm) {
            let element = PgType::parse(element_name)?;
            if !element.is_array_element() {
                return None;
            }
            return Some(PgType::Array(Box::new(element)));
        }
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
            "inet" => PgType::Inet,
            // `cidr` deliberately omitted — it always carries a mask
            // and is a different conceptual type from `inet`. Users
            // can fall through to the unsupported-type error.
            // `timestamp` / `timestamp without time zone` intentionally omitted.
            _ => return None,
        };
        Some(result)
    }

    /// Whether this type may serve as a native PG array element. Restricted
    /// to the primitives the binder/decoder can encode as `Vec<Option<T>>`.
    /// Excludes `bytea` (no `Vec<Option<Vec<u8>>>` array codec wired in),
    /// `xml`/`hll`/`inet`/`json`/`jsonb` (each needs a dedicated cast or
    /// codec the array path does not carry), and arrays themselves
    /// (nested arrays are unsupported).
    fn is_array_element(&self) -> bool {
        matches!(
            self,
            PgType::Bool
                | PgType::Int2
                | PgType::Int4
                | PgType::Int8
                | PgType::Float4
                | PgType::Float8
                | PgType::Text
                | PgType::Varchar
                | PgType::Bpchar
                | PgType::Date
                | PgType::TimestampTz
                | PgType::Uuid
                | PgType::Numeric
        )
    }
}

/// Extract the element type-name from a native PG array spelling, or `None`
/// when `name` is not an array. Handles the two introspection forms:
/// * `data_type` column: `<element>[]` (a single trailing `[]`; a multi-dim
///   `<element>[][]` leaves an inner `[]` that fails to re-parse, so nested
///   arrays correctly resolve to unsupported).
/// * `udt_name` column: `_<element>` (single leading underscore; the
///   double-underscore `__<element>` of a nested array leaves a leading
///   underscore that fails to re-parse).
fn array_element_name(name: &str) -> Option<&str> {
    if let Some(element) = name.strip_suffix("[]") {
        return Some(element.trim());
    }
    name.strip_prefix('_')
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
        PgType::Inet => DataType::Custom(Box::new(PgInetType)),
        // Native array: map the element through the same table. PG reports
        // the element's length / precision / scale modifiers on the array
        // column row, so the same args apply to the element conversion.
        // PG arrays always permit NULL elements, hence `element_nullable`.
        PgType::Array(element) => {
            let element_type =
                to_internal(*element, char_max_length, numeric_precision, numeric_scale);
            DataType::Array {
                element: Some(Box::new(element_type)),
                element_nullable: true,
            }
        }
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
    fn inet_parses_and_maps_to_custom() {
        assert_eq!(PgType::parse("inet"), Some(PgType::Inet));
        let dt = to_internal(PgType::Inet, None, None, None);
        match dt {
            DataType::Custom(t) => assert_eq!(t.kind(), "postgresql.inet"),
            other => panic!("expected DataType::Custom(inet), got {other:?}"),
        }
    }

    #[test]
    fn cidr_is_unsupported() {
        // `cidr` is structurally a separate concept; we deliberately
        // do not surface it as `inet` to avoid silently dropping the
        // mask semantics.
        assert!(PgType::parse("cidr").is_none());
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
    fn array_parses_from_both_introspection_forms() {
        // `data_type` column form
        assert_eq!(
            PgType::parse("int4[]"),
            Some(PgType::Array(Box::new(PgType::Int4)))
        );
        assert_eq!(
            PgType::parse("text[]"),
            Some(PgType::Array(Box::new(PgType::Text)))
        );
        // `udt_name` column form
        assert_eq!(
            PgType::parse("_int4"),
            Some(PgType::Array(Box::new(PgType::Int4)))
        );
        assert_eq!(
            PgType::parse("_timestamptz"),
            Some(PgType::Array(Box::new(PgType::TimestampTz)))
        );
        assert_eq!(
            PgType::parse("_numeric"),
            Some(PgType::Array(Box::new(PgType::Numeric)))
        );
    }

    #[test]
    fn array_maps_to_data_type_array_with_nullable_elements() {
        assert_eq!(
            to_internal(PgType::Array(Box::new(PgType::Int4)), None, None, None),
            DataType::Array {
                element: Some(Box::new(DataType::Int32)),
                element_nullable: true,
            }
        );
        // numeric element carries its precision/scale through
        assert_eq!(
            to_internal(
                PgType::Array(Box::new(PgType::Numeric)),
                None,
                Some(10),
                Some(2)
            ),
            DataType::Array {
                element: Some(Box::new(DataType::Decimal {
                    precision: Some(10),
                    scale: Some(2),
                })),
                element_nullable: true,
            }
        );
    }

    #[test]
    fn nested_array_is_unsupported() {
        // Multi-dimensional arrays are not bindable; both spellings reject.
        assert!(PgType::parse("int4[][]").is_none());
        assert!(PgType::parse("__int4").is_none());
    }

    #[test]
    fn array_of_unsupported_element_is_none() {
        // `bytea[]` / `json[]` / `inet[]` have no array codec wired in.
        assert!(PgType::parse("bytea[]").is_none());
        assert!(PgType::parse("_bytea").is_none());
        assert!(PgType::parse("json[]").is_none());
        assert!(PgType::parse("_inet").is_none());
        assert!(PgType::parse("xml[]").is_none());
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
