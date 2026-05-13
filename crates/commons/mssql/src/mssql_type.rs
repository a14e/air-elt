//! Canonical set of MS SQL column types the project recognises, plus the
//! one-way mapping `mssql → internal DataType`. The reverse mapping is
//! sink-specific and lives in `sinks/mssql`.

use air_elt_core::types::DataType;

use crate::types::image::MssqlImageType;
use crate::types::rowversion::MssqlRowVersionType;
use crate::types::time::MssqlTimeType;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MssqlType {
    Bit,
    TinyInt,
    SmallInt,
    Int,
    BigInt,
    Real,
    Float,
    Decimal,
    Money,
    SmallMoney,
    Char,
    VarChar,
    NChar,
    NVarChar,
    Text,
    NText,
    Binary,
    VarBinary,
    Image,
    Date,
    DateTime2,
    DateTime,
    SmallDateTime,
    UniqueIdentifier,
    Xml,
    RowVersion,
    Time,
}

/// Parse `DATA_TYPE` from `INFORMATION_SCHEMA.COLUMNS`. MS SQL stores the
/// type name cleanly — no `tinyint(1)` ambiguity like MySQL.
pub fn parse(data_type: &str) -> Option<MssqlType> {
    let dt = data_type.trim().to_ascii_lowercase();
    match dt.as_str() {
        "bit" => Some(MssqlType::Bit),
        "tinyint" => Some(MssqlType::TinyInt),
        "smallint" => Some(MssqlType::SmallInt),
        "int" | "integer" => Some(MssqlType::Int),
        "bigint" => Some(MssqlType::BigInt),
        "real" => Some(MssqlType::Real),
        "float" => Some(MssqlType::Float),
        "decimal" | "numeric" => Some(MssqlType::Decimal),
        "money" => Some(MssqlType::Money),
        "smallmoney" => Some(MssqlType::SmallMoney),
        "char" => Some(MssqlType::Char),
        "varchar" => Some(MssqlType::VarChar),
        "nchar" => Some(MssqlType::NChar),
        "nvarchar" => Some(MssqlType::NVarChar),
        "text" => Some(MssqlType::Text),
        "ntext" => Some(MssqlType::NText),
        "binary" => Some(MssqlType::Binary),
        "varbinary" => Some(MssqlType::VarBinary),
        "image" => Some(MssqlType::Image),
        "date" => Some(MssqlType::Date),
        "datetime2" => Some(MssqlType::DateTime2),
        "datetime" => Some(MssqlType::DateTime),
        "smalldatetime" => Some(MssqlType::SmallDateTime),
        "uniqueidentifier" => Some(MssqlType::UniqueIdentifier),
        "xml" => Some(MssqlType::Xml),
        "timestamp" | "rowversion" => Some(MssqlType::RowVersion),
        "time" => Some(MssqlType::Time),
        _ => None,
    }
}

/// Map an MS SQL type to the canonical `DataType`.
///
/// `char_max_length` comes from `INFORMATION_SCHEMA.COLUMNS.
/// CHARACTER_MAXIMUM_LENGTH`. MS SQL returns `-1` for MAX types.
///
/// `numeric_precision` and `numeric_scale` are read for `decimal`/`numeric`.
/// MS SQL always assigns concrete values (defaults `decimal(18, 0)`).
pub fn to_internal(
    mssql: MssqlType,
    char_max_length: Option<u32>,
    numeric_precision: Option<u32>,
    numeric_scale: Option<u32>,
) -> DataType {
    match mssql {
        MssqlType::Bit => DataType::Bool,
        MssqlType::TinyInt => DataType::UInt8,
        MssqlType::SmallInt => DataType::Int16,
        MssqlType::Int => DataType::Int32,
        MssqlType::BigInt => DataType::Int64,
        MssqlType::Real => DataType::Float32,
        MssqlType::Float => match numeric_precision {
            Some(p) if p <= 24 => DataType::Float32,
            _ => DataType::Float64,
        },
        MssqlType::Decimal => match (numeric_precision, numeric_scale) {
            (Some(p), Some(0)) => DataType::BigInt { width: Some(p) },
            (Some(p), Some(s)) => DataType::Decimal {
                precision: Some(p),
                scale: Some(s),
            },
            (Some(p), None) => DataType::BigInt { width: Some(p) },
            (None, _) => DataType::Decimal {
                precision: None,
                scale: None,
            },
        },
        MssqlType::Money => DataType::Decimal {
            precision: Some(19),
            scale: Some(4),
        },
        MssqlType::SmallMoney => DataType::Decimal {
            precision: Some(10),
            scale: Some(4),
        },
        MssqlType::Char
        | MssqlType::VarChar
        | MssqlType::NChar
        | MssqlType::NVarChar
        | MssqlType::Text
        | MssqlType::NText => DataType::Text {
            size: char_max_length,
        },
        MssqlType::Binary | MssqlType::VarBinary => DataType::Bytes {
            size: char_max_length,
        },
        MssqlType::Image => DataType::Custom(Box::new(MssqlImageType)),
        MssqlType::Date => DataType::Date,
        MssqlType::DateTime2 | MssqlType::DateTime | MssqlType::SmallDateTime => {
            DataType::Timestamp
        }
        MssqlType::UniqueIdentifier => DataType::Uuid,
        MssqlType::Xml => DataType::Xml,
        MssqlType::RowVersion => DataType::Custom(Box::new(MssqlRowVersionType)),
        MssqlType::Time => DataType::Custom(Box::new(MssqlTimeType)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn bit_is_bool() {
        assert_eq!(parse("bit"), Some(MssqlType::Bit));
        assert_eq!(
            to_internal(MssqlType::Bit, None, None, None),
            DataType::Bool
        );
    }

    #[test]
    fn tinyint_is_uint8() {
        assert_eq!(parse("tinyint"), Some(MssqlType::TinyInt));
        assert_eq!(
            to_internal(MssqlType::TinyInt, None, None, None),
            DataType::UInt8
        );
    }

    #[test]
    fn smallint_is_int16() {
        assert_eq!(parse("smallint"), Some(MssqlType::SmallInt));
        assert_eq!(
            to_internal(MssqlType::SmallInt, None, None, None),
            DataType::Int16
        );
    }

    #[test]
    fn int_is_int32() {
        assert_eq!(parse("int"), Some(MssqlType::Int));
        assert_eq!(parse("integer"), Some(MssqlType::Int));
        assert_eq!(
            to_internal(MssqlType::Int, None, None, None),
            DataType::Int32
        );
    }

    #[test]
    fn bigint_is_int64() {
        assert_eq!(parse("bigint"), Some(MssqlType::BigInt));
        assert_eq!(
            to_internal(MssqlType::BigInt, None, None, None),
            DataType::Int64
        );
    }

    #[test]
    fn real_is_float32() {
        assert_eq!(parse("real"), Some(MssqlType::Real));
        assert_eq!(
            to_internal(MssqlType::Real, None, None, None),
            DataType::Float32
        );
    }

    #[test]
    fn float_24_is_float32() {
        assert_eq!(parse("float"), Some(MssqlType::Float));
        assert_eq!(
            to_internal(MssqlType::Float, None, Some(24), None),
            DataType::Float32
        );
    }

    #[test]
    fn float_53_is_float64() {
        assert_eq!(
            to_internal(MssqlType::Float, None, Some(53), None),
            DataType::Float64
        );
    }

    #[test]
    fn float_no_precision_defaults_float64() {
        assert_eq!(
            to_internal(MssqlType::Float, None, None, None),
            DataType::Float64
        );
    }

    #[test]
    fn decimal_zero_scale_is_bigint() {
        assert_eq!(parse("decimal"), Some(MssqlType::Decimal));
        assert_eq!(parse("numeric"), Some(MssqlType::Decimal));
        assert_eq!(
            to_internal(MssqlType::Decimal, None, Some(20), Some(0)),
            DataType::BigInt { width: Some(20) }
        );
    }

    #[test]
    fn decimal_with_scale_is_decimal() {
        assert_eq!(
            to_internal(MssqlType::Decimal, None, Some(10), Some(2)),
            DataType::Decimal {
                precision: Some(10),
                scale: Some(2)
            }
        );
    }

    #[test]
    fn money_is_decimal_19_4() {
        assert_eq!(parse("money"), Some(MssqlType::Money));
        assert_eq!(
            to_internal(MssqlType::Money, None, None, None),
            DataType::Decimal {
                precision: Some(19),
                scale: Some(4)
            }
        );
    }

    #[test]
    fn smallmoney_is_decimal_10_4() {
        assert_eq!(parse("smallmoney"), Some(MssqlType::SmallMoney));
        assert_eq!(
            to_internal(MssqlType::SmallMoney, None, None, None),
            DataType::Decimal {
                precision: Some(10),
                scale: Some(4)
            }
        );
    }

    #[test]
    fn varchar_carries_size() {
        assert_eq!(parse("varchar"), Some(MssqlType::VarChar));
        let dt = to_internal(MssqlType::VarChar, Some(255), None, None);
        assert_eq!(dt, DataType::Text { size: Some(255) });
    }

    #[test]
    fn nvarchar_max_is_unbounded_text() {
        assert_eq!(parse("nvarchar"), Some(MssqlType::NVarChar));
        let dt = to_internal(MssqlType::NVarChar, Some(u32::MAX), None, None);
        // MAX types carry -1 from information_schema; we pass it through as
        // the raw char_max_length. The sink layer handles the bound.
        assert!(matches!(dt, DataType::Text { .. }));
    }

    #[test]
    fn text_is_unbounded_text() {
        assert_eq!(parse("text"), Some(MssqlType::Text));
        let dt = to_internal(MssqlType::Text, None, None, None);
        assert_eq!(dt, DataType::Text { size: None });
    }

    #[test]
    fn date_is_date() {
        assert_eq!(parse("date"), Some(MssqlType::Date));
        assert_eq!(
            to_internal(MssqlType::Date, None, None, None),
            DataType::Date
        );
    }

    #[test]
    fn datetime2_is_timestamp() {
        assert_eq!(parse("datetime2"), Some(MssqlType::DateTime2));
        assert_eq!(
            to_internal(MssqlType::DateTime2, None, None, None),
            DataType::Timestamp
        );
    }

    #[test]
    fn datetime_is_timestamp() {
        assert_eq!(parse("datetime"), Some(MssqlType::DateTime));
        assert_eq!(
            to_internal(MssqlType::DateTime, None, None, None),
            DataType::Timestamp
        );
    }

    #[test]
    fn uniqueidentifier_is_uuid() {
        assert_eq!(parse("uniqueidentifier"), Some(MssqlType::UniqueIdentifier));
        assert_eq!(
            to_internal(MssqlType::UniqueIdentifier, None, None, None),
            DataType::Uuid
        );
    }

    #[test]
    fn xml_is_xml() {
        assert_eq!(parse("xml"), Some(MssqlType::Xml));
        assert_eq!(to_internal(MssqlType::Xml, None, None, None), DataType::Xml);
    }

    #[test]
    fn rowversion_is_custom() {
        assert_eq!(parse("rowversion"), Some(MssqlType::RowVersion));
        assert_eq!(parse("timestamp"), Some(MssqlType::RowVersion));
        let dt = to_internal(MssqlType::RowVersion, None, None, None);
        assert!(matches!(dt, DataType::Custom(_)));
        if let DataType::Custom(t) = dt {
            assert_eq!(t.kind(), "mssql.rowversion");
            assert_eq!(t.fixed_size(), Some(8));
            assert!(!t.can_be_cursor());
        }
    }

    #[test]
    fn image_is_custom() {
        assert_eq!(parse("image"), Some(MssqlType::Image));
        let dt = to_internal(MssqlType::Image, None, None, None);
        assert!(matches!(dt, DataType::Custom(_)));
        if let DataType::Custom(t) = dt {
            assert_eq!(t.kind(), "mssql.image");
            assert!(!t.can_be_cursor());
        }
    }

    #[test]
    fn unknown_type_is_none() {
        assert!(parse("geography").is_none());
        assert!(parse("hierarchyid").is_none());
        assert!(parse("datetimeoffset").is_none());
        assert!(parse("sql_variant").is_none());
    }
}
