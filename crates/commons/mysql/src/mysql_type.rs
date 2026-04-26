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
            } else {
                MySqlType::TinyInt
            }
        }
        "smallint" => MySqlType::SmallInt,
        "mediumint" => MySqlType::MediumInt,
        "int" | "integer" => MySqlType::Int,
        "bigint" => MySqlType::BigInt,
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
        // datetime / time / year intentionally omitted.
        _ => return None,
    };
    Some(result)
}

/// Map a MySQL type to the canonical `DataType`. `char_max_length` comes
/// from `information_schema.COLUMNS.CHARACTER_MAXIMUM_LENGTH`. For the
/// fixed-width variants (`tinytext` etc.) we hard-code the size limits per
/// the MySQL reference.
pub fn to_internal(mysql: MySqlType, char_max_length: Option<u32>) -> DataType {
    match mysql {
        MySqlType::Bool => DataType::Bool,
        MySqlType::TinyInt | MySqlType::SmallInt => DataType::Int16,
        MySqlType::MediumInt | MySqlType::Int => DataType::Int32,
        MySqlType::BigInt => DataType::Int64,
        MySqlType::Float => DataType::Float32,
        MySqlType::Double => DataType::Float64,
        MySqlType::Char | MySqlType::VarChar => DataType::Text {
            size: char_max_length,
        },
        MySqlType::TinyText => DataType::Text { size: Some(255) },
        MySqlType::Text | MySqlType::MediumText | MySqlType::LongText => {
            DataType::Text { size: None }
        }
        MySqlType::Binary | MySqlType::VarBinary => DataType::Bytes {
            size: char_max_length,
        },
        MySqlType::TinyBlob => DataType::Bytes { size: Some(255) },
        MySqlType::Blob | MySqlType::MediumBlob | MySqlType::LongBlob => {
            DataType::Bytes { size: None }
        }
        MySqlType::Date => DataType::Date,
        MySqlType::Timestamp => DataType::Timestamp,
        MySqlType::Json => DataType::Json,
        MySqlType::Uuid => DataType::Uuid,
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
    }

    #[test]
    fn datetime_rejected() {
        assert!(parse("datetime", "datetime").is_none());
    }

    #[test]
    fn varchar_carries_size() {
        let dt = to_internal(MySqlType::VarChar, Some(64));
        assert_eq!(dt, DataType::Text { size: Some(64) });
    }

    #[test]
    fn longtext_unbounded() {
        let dt = to_internal(MySqlType::LongText, None);
        assert_eq!(dt, DataType::Text { size: None });
    }

    #[test]
    fn tinytext_size_255() {
        assert_eq!(
            to_internal(MySqlType::TinyText, None),
            DataType::Text { size: Some(255) }
        );
    }

    #[test]
    fn binary_carries_size() {
        assert_eq!(
            to_internal(MySqlType::Binary, Some(16)),
            DataType::Bytes { size: Some(16) }
        );
    }

    #[test]
    fn json_maps_to_json() {
        assert_eq!(parse("json", "json"), Some(MySqlType::Json));
        assert_eq!(to_internal(MySqlType::Json, None), DataType::Json);
    }

    #[test]
    fn timestamp_maps_to_timestamp() {
        assert_eq!(parse("timestamp", "timestamp"), Some(MySqlType::Timestamp));
        assert_eq!(to_internal(MySqlType::Timestamp, None), DataType::Timestamp);
    }

    #[test]
    fn mariadb_native_uuid() {
        assert_eq!(parse("uuid", "uuid"), Some(MySqlType::Uuid));
        assert_eq!(to_internal(MySqlType::Uuid, None), DataType::Uuid);
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(parse("geometry", "geometry").is_none());
        assert!(parse("year", "year(4)").is_none());
    }
}
