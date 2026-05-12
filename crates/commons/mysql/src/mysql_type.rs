//! Canonical set of MySQL column types the project recognises, plus the
//! one-way mapping `mysql → internal DataType`. The reverse mapping is sink-
//! specific and lives in `sinks/mysql`.
//!
//! Why parsing both `data_type` and `column_type`: only `column_type`
//! preserves the `(N)` width and the `tinyint(1)` discriminator that we
//! need to distinguish booleans from small integers.

use air_elt_core::types::DataType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MySqlType {
    /// `tinyint(1)` only. Other tinyints fall under `TinyInt`.
    Bool,
    TinyInt,
    SmallInt,
    MediumInt,
    Int,
    BigInt,
    /// `tinyint UNSIGNED` (range 0..255).
    TinyIntUnsigned,
    /// `smallint UNSIGNED` (range 0..65535).
    SmallIntUnsigned,
    /// `mediumint UNSIGNED` (range 0..16M).
    MediumIntUnsigned,
    /// `int UNSIGNED` (range 0..4G).
    IntUnsigned,
    /// `bigint UNSIGNED` (range 0..18Q). Wider than i64 — needs `UInt64`.
    BigIntUnsigned,
    Float,
    Double,
    Char,
    VarChar,
    TinyText,
    Text,
    MediumText,
    LongText,
    Binary,
    VarBinary,
    TinyBlob,
    Blob,
    MediumBlob,
    LongBlob,
    Date,
    Timestamp,
    Json,
    /// Native UUID column. **MariaDB 10.7+ only** — MySQL has no UUID type
    /// (use `CHAR(36)` or `BINARY(16)` with the conversion layer instead).
    Uuid,
    /// `decimal(p, s)` / `numeric(p, s)`. Both p and s are always defined in
    /// MySQL/MariaDB; precision/scale fall through `to_internal` to pick
    /// `BigInt` (s = 0) vs `Decimal` (s > 0).
    Decimal,
}

/// Parse an `information_schema.columns` row.
///
/// `data_type` carries the lowercased base type (`tinyint`, `varchar`, …)
/// and `column_type` carries the full text (`tinyint(1)`, `varchar(255)`)
/// — we need both to honour the boolean/tinyint split.
///
/// `datetime` is intentionally rejected, mirroring pg's stance on `timestamp
/// without time zone`. Use `TIMESTAMP` instead.
pub fn parse(data_type: &str, column_type: &str) -> Option<MySqlType> {
    let dt = data_type.trim().to_ascii_lowercase();
    let ct = column_type.trim().to_ascii_lowercase();
    // The `unsigned` modifier only appears in `column_type`, not `data_type`.
    // We treat `zerofill` as unsigned-implying because MySQL stores zerofill
    // values without sign too.
    let unsigned = ct
        .split_whitespace()
        .any(|tok| tok == "unsigned" || tok == "zerofill");
    let result = match dt.as_str() {
        "tinyint" => {
            // ORMs sometimes emit `tinyint(1) unsigned` / `tinyint(1) zerofill`
            // for boolean columns; honour the boolean intent.
            if ct.starts_with("tinyint(1)")
                && ct[10..]
                    .split_whitespace()
                    .all(|tok| matches!(tok, "unsigned" | "zerofill"))
            {
                MySqlType::Bool
            } else if unsigned {
                MySqlType::TinyIntUnsigned
            } else {
                MySqlType::TinyInt
            }
        }
        "smallint" => {
            if unsigned {
                MySqlType::SmallIntUnsigned
            } else {
                MySqlType::SmallInt
            }
        }
        "mediumint" => {
            if unsigned {
                MySqlType::MediumIntUnsigned
            } else {
                MySqlType::MediumInt
            }
        }
        "int" | "integer" => {
            if unsigned {
                MySqlType::IntUnsigned
            } else {
                MySqlType::Int
            }
        }
        "bigint" => {
            if unsigned {
                MySqlType::BigIntUnsigned
            } else {
                MySqlType::BigInt
            }
        }
        "float" => MySqlType::Float,
        "double" | "double precision" | "real" => MySqlType::Double,
        "char" => MySqlType::Char,
        "varchar" => MySqlType::VarChar,
        "tinytext" => MySqlType::TinyText,
        "text" => MySqlType::Text,
        "mediumtext" => MySqlType::MediumText,
        "longtext" => MySqlType::LongText,
        "binary" => MySqlType::Binary,
        "varbinary" => MySqlType::VarBinary,
        "tinyblob" => MySqlType::TinyBlob,
        "blob" => MySqlType::Blob,
        "mediumblob" => MySqlType::MediumBlob,
        "longblob" => MySqlType::LongBlob,
        "date" => MySqlType::Date,
        "timestamp" => MySqlType::Timestamp,
        "json" => MySqlType::Json,
        // Native UUID — MariaDB 10.7+ only. Surfaces in `INFORMATION_SCHEMA.
        // COLUMNS.DATA_TYPE` as literal `uuid`. MySQL stores UUIDs as
        // CHAR/BINARY and never reaches this arm.
        "uuid" => MySqlType::Uuid,
        "decimal" | "numeric" => MySqlType::Decimal,
        // datetime / time / year intentionally omitted.
        _ => return None,
    };
    Some(result)
}

/// Map a MySQL type to the canonical `DataType`.
///
/// `char_max_length` comes from `information_schema.COLUMNS.
/// CHARACTER_MAXIMUM_LENGTH`. For the fixed-width variants (`tinytext` etc.)
/// we hard-code the size limits per the MySQL reference.
///
/// `numeric_precision` / `numeric_scale` are read for `decimal`/`numeric`
/// columns. MySQL always assigns concrete values (defaults `decimal(10, 0)`).
pub fn to_internal(
    mysql: MySqlType,
    char_max_length: Option<u32>,
    numeric_precision: Option<u32>,
    numeric_scale: Option<u32>,
) -> DataType {
    match mysql {
        MySqlType::Bool => DataType::Bool,
        // MySQL `tinyint` (signed, not tinyint(1)) is 1 byte: i8::MIN..=i8::MAX.
        // `Int8` is the precise canonical type; Int16 was previously used but
        // would waste a byte and misrepresent the actual range.
        MySqlType::TinyInt => DataType::Int8,
        MySqlType::SmallInt => DataType::Int16,
        MySqlType::MediumInt | MySqlType::Int => DataType::Int32,
        MySqlType::BigInt => DataType::Int64,
        MySqlType::TinyIntUnsigned => DataType::UInt8,
        MySqlType::SmallIntUnsigned => DataType::UInt16,
        // Both fit in u32 (mediumint unsigned tops out at 2^24 − 1).
        MySqlType::MediumIntUnsigned | MySqlType::IntUnsigned => DataType::UInt32,
        MySqlType::BigIntUnsigned => DataType::UInt64,
        MySqlType::Float => DataType::Float32,
        MySqlType::Double => DataType::Float64,
        MySqlType::Char | MySqlType::VarChar => DataType::Text {
            size: char_max_length,
        },
        MySqlType::TinyText => DataType::Text { size: Some(255) },
        // MySQL `text` family carries a fixed maximum-byte size per the
        // reference — surface them as bounded `Text { size }` so the matrix
        // can apply concrete narrowing checks. `text` itself is 64 KiB-1.
        MySqlType::Text => DataType::Text { size: Some(65_535) },
        MySqlType::MediumText => DataType::Text {
            size: Some(16_777_215),
        },
        MySqlType::LongText => DataType::Text {
            size: Some(4_294_967_295),
        },
        MySqlType::Binary | MySqlType::VarBinary => DataType::Bytes {
            size: char_max_length,
        },
        MySqlType::TinyBlob => DataType::Bytes { size: Some(255) },
        MySqlType::Blob => DataType::Bytes { size: Some(65_535) },
        MySqlType::MediumBlob => DataType::Bytes {
            size: Some(16_777_215),
        },
        MySqlType::LongBlob => DataType::Bytes {
            size: Some(4_294_967_295),
        },
        MySqlType::Date => DataType::Date,
        MySqlType::Timestamp => DataType::Timestamp,
        MySqlType::Json => DataType::Json,
        MySqlType::Uuid => DataType::Uuid,
        MySqlType::Decimal => match (numeric_precision, numeric_scale) {
            (Some(p), Some(0)) => DataType::BigInt { width: Some(p) },
            (Some(p), Some(s)) => DataType::Decimal {
                precision: Some(p),
                scale: Some(s),
            },
            // Precision-only fallback (non-canonical in MySQL — kept symmetric
            // with the pg side: scale-unset is treated as scale 0 → BigInt
            // with the declared digit width).
            (Some(p), None) => DataType::BigInt { width: Some(p) },
            // No precision — fall through as fully-unbounded decimal.
            (None, _) => DataType::Decimal {
                precision: None,
                scale: None,
            },
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn tinyint_one_is_bool() {
        assert_eq!(parse("tinyint", "tinyint(1)"), Some(MySqlType::Bool));
    }

    #[test]
    fn tinyint_one_unsigned_is_bool() {
        assert_eq!(
            parse("tinyint", "tinyint(1) unsigned"),
            Some(MySqlType::Bool)
        );
        assert_eq!(
            parse("tinyint", "tinyint(1) unsigned zerofill"),
            Some(MySqlType::Bool)
        );
    }

    #[test]
    fn tinyint_other_widths_are_int() {
        assert_eq!(parse("tinyint", "tinyint(4)"), Some(MySqlType::TinyInt));
        assert_eq!(parse("tinyint", "tinyint"), Some(MySqlType::TinyInt));
        // TinyInt (signed, not tinyint(1)) maps to Int8, not Int16.
        assert_eq!(
            to_internal(MySqlType::TinyInt, None, None, None),
            DataType::Int8
        );
    }

    #[test]
    fn datetime_rejected() {
        assert!(parse("datetime", "datetime").is_none());
    }

    #[test]
    fn varchar_carries_size() {
        let dt = to_internal(MySqlType::VarChar, Some(64), None, None);
        assert_eq!(dt, DataType::Text { size: Some(64) });
    }

    #[test]
    fn longtext_carries_concrete_size() {
        let dt = to_internal(MySqlType::LongText, None, None, None);
        assert_eq!(
            dt,
            DataType::Text {
                size: Some(4_294_967_295),
            }
        );
    }

    #[test]
    fn mediumtext_carries_concrete_size() {
        assert_eq!(
            to_internal(MySqlType::MediumText, None, None, None),
            DataType::Text {
                size: Some(16_777_215),
            }
        );
    }

    #[test]
    fn text_carries_64k_size() {
        assert_eq!(
            to_internal(MySqlType::Text, None, None, None),
            DataType::Text { size: Some(65_535) }
        );
    }

    #[test]
    fn tinytext_size_255() {
        assert_eq!(
            to_internal(MySqlType::TinyText, None, None, None),
            DataType::Text { size: Some(255) }
        );
    }

    #[test]
    fn binary_carries_size() {
        assert_eq!(
            to_internal(MySqlType::Binary, Some(16), None, None),
            DataType::Bytes { size: Some(16) }
        );
    }

    #[test]
    fn json_maps_to_json() {
        assert_eq!(parse("json", "json"), Some(MySqlType::Json));
        assert_eq!(
            to_internal(MySqlType::Json, None, None, None),
            DataType::Json
        );
    }

    #[test]
    fn timestamp_maps_to_timestamp() {
        assert_eq!(parse("timestamp", "timestamp"), Some(MySqlType::Timestamp));
        assert_eq!(
            to_internal(MySqlType::Timestamp, None, None, None),
            DataType::Timestamp
        );
    }

    #[test]
    fn mariadb_native_uuid() {
        assert_eq!(parse("uuid", "uuid"), Some(MySqlType::Uuid));
        assert_eq!(
            to_internal(MySqlType::Uuid, None, None, None),
            DataType::Uuid
        );
    }

    #[test]
    fn decimal_zero_scale_is_bigint() {
        assert_eq!(parse("decimal", "decimal(20,0)"), Some(MySqlType::Decimal));
        assert_eq!(
            to_internal(MySqlType::Decimal, None, Some(20), Some(0)),
            DataType::BigInt { width: Some(20) }
        );
    }

    #[test]
    fn decimal_with_scale_is_decimal() {
        assert_eq!(
            to_internal(MySqlType::Decimal, None, Some(10), Some(2)),
            DataType::Decimal {
                precision: Some(10),
                scale: Some(2)
            }
        );
    }

    #[test]
    fn unsigned_int_variants() {
        assert_eq!(
            parse("tinyint", "tinyint(3) unsigned"),
            Some(MySqlType::TinyIntUnsigned)
        );
        assert_eq!(
            parse("smallint", "smallint unsigned"),
            Some(MySqlType::SmallIntUnsigned)
        );
        assert_eq!(
            parse("mediumint", "mediumint unsigned"),
            Some(MySqlType::MediumIntUnsigned)
        );
        assert_eq!(
            parse("int", "int(11) unsigned"),
            Some(MySqlType::IntUnsigned)
        );
        assert_eq!(
            parse("bigint", "bigint(20) unsigned"),
            Some(MySqlType::BigIntUnsigned)
        );
        assert_eq!(
            to_internal(MySqlType::TinyIntUnsigned, None, None, None),
            DataType::UInt8
        );
        assert_eq!(
            to_internal(MySqlType::SmallIntUnsigned, None, None, None),
            DataType::UInt16
        );
        // Mediumint unsigned (3-byte, max 2^24-1) shares UInt32 with int unsigned.
        assert_eq!(
            to_internal(MySqlType::MediumIntUnsigned, None, None, None),
            DataType::UInt32
        );
        assert_eq!(
            to_internal(MySqlType::IntUnsigned, None, None, None),
            DataType::UInt32
        );
        assert_eq!(
            to_internal(MySqlType::BigIntUnsigned, None, None, None),
            DataType::UInt64
        );
    }

    #[test]
    fn decimal_precision_only_falls_back_to_bigint() {
        // Symmetric with pg_type: precision without scale → BigInt(width).
        assert_eq!(
            to_internal(MySqlType::Decimal, None, Some(20), None),
            DataType::BigInt { width: Some(20) }
        );
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(parse("geometry", "geometry").is_none());
        assert!(parse("year", "year(4)").is_none());
    }
}
