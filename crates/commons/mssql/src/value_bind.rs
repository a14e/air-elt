//! MS SQL value-binding bridge.
//!
//! Converts a canonical `&Value` (+ target `DataType` for nullability typing)
//! into a `tiberius::ColumnData<'static>` that can be passed as a parameter
//! to `Client::query` / `Client::execute` / `BulkLoadRequest::send`.
//!
//! Mirrors the role of `air-elt-commons-pg::sink_bind::bind_value_separated`
//! for the tiberius driver. Every sink/source write path must route values
//! through this function — direct SQL interpolation is forbidden by
//! `project-conventions::SQL helpers`.

use std::borrow::Cow;

use bigdecimal::BigDecimal;
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use tiberius::numeric::Numeric;
use tiberius::{ColumnData, ToSql};

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::types::{DataType, Value};

use crate::types::image::{MssqlImageType, MssqlImageValue};
use crate::types::rowversion::{MssqlRowVersionType, MssqlRowVersionValue};
use crate::types::time::{MssqlTimeType, MssqlTimeValue};

/// Convert a `&Value` into a tiberius `ColumnData<'static>`.
///
/// The `dt` argument disambiguates `Value::Null` so the right typed-NULL
/// variant is produced (TDS requires the type for NULL parameters).
pub fn value_to_column_data(v: &Value, dt: &DataType) -> RuntimeResult<ColumnData<'static>> {
    match v {
        Value::Null => null_for(dt),
        Value::Bool(b) => Ok(ColumnData::Bit(Some(*b))),
        Value::Int8(n) => Ok(ColumnData::I16(Some(i16::from(*n)))),
        Value::Int16(n) => Ok(ColumnData::I16(Some(*n))),
        Value::Int32(n) => Ok(ColumnData::I32(Some(*n))),
        Value::Int64(n) => Ok(ColumnData::I64(Some(*n))),
        Value::UInt8(n) => Ok(ColumnData::U8(Some(*n))),
        Value::UInt16(n) => Ok(ColumnData::I32(Some(i32::from(*n)))),
        Value::UInt32(n) => Ok(ColumnData::I64(Some(i64::from(*n)))),
        Value::UInt64(n) => {
            let signed = i64::try_from(*n).map_err(|_| {
                RuntimeError::Other(format!(
                    "mssql cannot bind UInt64 value {n}: exceeds i64::MAX"
                ))
            })?;
            Ok(ColumnData::I64(Some(signed)))
        }
        Value::Float32(n) => {
            if n.is_nan() || n.is_infinite() {
                return Err(RuntimeError::Other(
                    "mssql cannot bind float NaN/Infinity".into(),
                ));
            }
            Ok(ColumnData::F32(Some(*n)))
        }
        Value::Float64(n) => {
            if n.is_nan() || n.is_infinite() {
                return Err(RuntimeError::Other(
                    "mssql cannot bind float NaN/Infinity".into(),
                ));
            }
            Ok(ColumnData::F64(Some(*n)))
        }
        Value::Text(s) => Ok(ColumnData::String(Some(Cow::Owned(s.clone())))),
        Value::Bytes(b) => Ok(ColumnData::Binary(Some(Cow::Owned(b.clone())))),
        Value::Date(d) => {
            // Tiberius `to_sql` impl for NaiveDate maps to `ColumnData::Date`.
            let cd = tiberius::ToSql::to_sql(d);
            Ok(strip_lifetime(cd))
        }
        Value::Timestamp(ts) => {
            let naive = ts.naive_utc();
            let cd = tiberius::ToSql::to_sql(&naive);
            Ok(strip_lifetime(cd))
        }
        Value::Uuid(u) => Ok(ColumnData::Guid(Some(*u))),
        Value::BigInt(b) => Ok(ColumnData::Numeric(Some(bigint_to_numeric(b)?))),
        Value::Decimal(d) => Ok(ColumnData::Numeric(Some(decimal_to_numeric(d)?))),
        Value::Json(j) => {
            // MSSQL has no native JSON type; map to NVARCHAR(MAX) as a string.
            let s = serde_json::to_string(j)
                .map_err(|e| RuntimeError::Other(format!("mssql json bind serialise: {e}")))?;
            Ok(ColumnData::String(Some(Cow::Owned(s))))
        }
        Value::Custom(c) => {
            let any = c.as_any();
            if let Some(img) = any.downcast_ref::<MssqlImageValue>() {
                return Ok(ColumnData::Binary(Some(Cow::Owned(img.0.clone()))));
            }
            if any.downcast_ref::<MssqlRowVersionValue>().is_some() {
                return Err(RuntimeError::Other(
                    "mssql rowversion is read-only and cannot be bound for insert/update".into(),
                ));
            }
            if let Some(t) = any.downcast_ref::<MssqlTimeValue>() {
                let cd = tiberius::ToSql::to_sql(&t.0);
                return Ok(strip_lifetime(cd));
            }
            Err(RuntimeError::Other(format!(
                "mssql cannot bind Custom value of kind {:?}",
                c.dyn_type().kind()
            )))
        }
    }
}

/// Build a typed NULL `ColumnData` based on the target `DataType`.
fn null_for(dt: &DataType) -> RuntimeResult<ColumnData<'static>> {
    match dt {
        DataType::Bool => Ok(ColumnData::Bit(None)),
        DataType::Int8 => Ok(ColumnData::I16(None)),
        DataType::Int16 => Ok(ColumnData::I16(None)),
        DataType::Int32 => Ok(ColumnData::I32(None)),
        DataType::Int64 => Ok(ColumnData::I64(None)),
        DataType::UInt8 => Ok(ColumnData::U8(None)),
        DataType::UInt16 => Ok(ColumnData::I32(None)),
        DataType::UInt32 | DataType::UInt64 => Ok(ColumnData::I64(None)),
        DataType::Float32 => Ok(ColumnData::F32(None)),
        DataType::Float64 => Ok(ColumnData::F64(None)),
        DataType::Text { .. } | DataType::Xml | DataType::Json => Ok(ColumnData::String(None)),
        DataType::Bytes { .. } => Ok(ColumnData::Binary(None)),
        DataType::Date => Ok(ColumnData::Date(None)),
        DataType::Timestamp => Ok(ColumnData::DateTime2(None)),
        DataType::Uuid => Ok(ColumnData::Guid(None)),
        DataType::BigInt { .. } | DataType::Decimal { .. } => Ok(ColumnData::Numeric(None)),
        DataType::Custom(ct) if ct.kind() == MssqlImageType::KIND => Ok(ColumnData::Binary(None)),
        DataType::Custom(ct) if ct.kind() == MssqlRowVersionType::KIND => {
            // ROWVERSION columns must be excluded from INSERTs entirely; we
            // still return a typed NULL here for completeness in case a
            // caller binds one (the server would reject the write anyway).
            Ok(ColumnData::Binary(None))
        }
        DataType::Custom(ct) if ct.kind() == MssqlTimeType::KIND => Ok(ColumnData::Time(None)),
        DataType::Custom(ct) => Err(RuntimeError::Other(format!(
            "mssql cannot bind NULL for Custom type {:?}",
            ct.kind()
        ))),
        DataType::Union(_) => Err(RuntimeError::Other(
            "mssql cannot bind NULL for Union type".into(),
        )),
    }
}

/// Convert a `BigDecimal` (scale-aware) to a tiberius `Numeric`.
fn decimal_to_numeric(d: &BigDecimal) -> RuntimeResult<Numeric> {
    let (coef, scale) = d.clone().into_bigint_and_exponent();
    let scale_u8 = u8::try_from(scale).map_err(|_| {
        RuntimeError::Other(format!(
            "mssql cannot bind decimal: scale {scale} exceeds u8 range"
        ))
    })?;
    let value = coef.to_i128().ok_or_else(|| {
        RuntimeError::Other(format!(
            "mssql cannot bind decimal: coefficient does not fit in i128 ({coef})"
        ))
    })?;
    Ok(Numeric::new_with_scale(value, scale_u8))
}

/// Convert a `num_bigint::BigInt` (scale-zero) to a tiberius `Numeric`.
fn bigint_to_numeric(b: &BigInt) -> RuntimeResult<Numeric> {
    let value = b.to_i128().ok_or_else(|| {
        RuntimeError::Other(format!(
            "mssql cannot bind BigInt: value does not fit in i128 ({b})"
        ))
    })?;
    Ok(Numeric::new_with_scale(value, 0))
}

/// Wrapper that lets us pass a pre-built `ColumnData<'static>` as a
/// `&dyn ToSql` parameter to `Client::query` / `execute`. The bb8/tiberius
/// API requires `&[&dyn ToSql]`; `ColumnData` itself does not implement
/// `ToSql`, so we clone it on each `to_sql` call.
pub struct BoundValue(pub ColumnData<'static>);

impl ToSql for BoundValue {
    fn to_sql(&self) -> ColumnData<'_> {
        self.0.clone()
    }
}

/// Force a `ColumnData<'_>` produced by `tiberius::ToSql::to_sql` into the
/// `'static` lifetime. Safe because our `Value` arms only ever use owned
/// (`Cow::Owned`) payloads or `Copy` ones — never borrowed references that
/// would dangle.
fn strip_lifetime(cd: ColumnData<'_>) -> ColumnData<'static> {
    match cd {
        ColumnData::U8(v) => ColumnData::U8(v),
        ColumnData::I16(v) => ColumnData::I16(v),
        ColumnData::I32(v) => ColumnData::I32(v),
        ColumnData::I64(v) => ColumnData::I64(v),
        ColumnData::F32(v) => ColumnData::F32(v),
        ColumnData::F64(v) => ColumnData::F64(v),
        ColumnData::Bit(v) => ColumnData::Bit(v),
        ColumnData::String(v) => ColumnData::String(v.map(|c| Cow::Owned(c.into_owned()))),
        ColumnData::Guid(v) => ColumnData::Guid(v),
        ColumnData::Binary(v) => ColumnData::Binary(v.map(|c| Cow::Owned(c.into_owned()))),
        ColumnData::Numeric(v) => ColumnData::Numeric(v),
        ColumnData::Xml(v) => ColumnData::Xml(v.map(|c| Cow::Owned(c.into_owned()))),
        ColumnData::DateTime(v) => ColumnData::DateTime(v),
        ColumnData::SmallDateTime(v) => ColumnData::SmallDateTime(v),
        ColumnData::Time(v) => ColumnData::Time(v),
        ColumnData::Date(v) => ColumnData::Date(v),
        ColumnData::DateTime2(v) => ColumnData::DateTime2(v),
        ColumnData::DateTimeOffset(v) => ColumnData::DateTimeOffset(v),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
    use std::str::FromStr;

    #[test]
    fn bool_value() {
        let cd = value_to_column_data(&Value::Bool(true), &DataType::Bool).unwrap();
        assert!(matches!(cd, ColumnData::Bit(Some(true))));
    }

    #[test]
    fn int32_value() {
        let cd = value_to_column_data(&Value::Int32(7), &DataType::Int32).unwrap();
        assert!(matches!(cd, ColumnData::I32(Some(7))));
    }

    #[test]
    fn uint64_overflow_errors() {
        let v = Value::UInt64(u64::MAX);
        let err = value_to_column_data(&v, &DataType::Int64).unwrap_err();
        assert!(format!("{err}").contains("UInt64"));
    }

    #[test]
    fn float_nan_errors() {
        let v = Value::Float64(f64::NAN);
        let err = value_to_column_data(&v, &DataType::Float64).unwrap_err();
        assert!(format!("{err}").contains("NaN/Infinity"));
    }

    #[test]
    fn float_infinity_errors() {
        let v = Value::Float32(f32::INFINITY);
        assert!(value_to_column_data(&v, &DataType::Float32).is_err());
    }

    #[test]
    fn text_value() {
        let cd = value_to_column_data(&Value::Text("héllo".into()), &DataType::Text { size: None })
            .unwrap();
        match cd {
            ColumnData::String(Some(s)) => assert_eq!(&*s, "héllo"),
            _ => panic!("expected String"),
        }
    }

    #[test]
    fn bytes_value() {
        let cd = value_to_column_data(
            &Value::Bytes(vec![1, 2, 3]),
            &DataType::Bytes { size: None },
        )
        .unwrap();
        match cd {
            ColumnData::Binary(Some(b)) => assert_eq!(&*b, &[1, 2, 3]),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn date_value() {
        let d = NaiveDate::from_ymd_opt(2025, 5, 13).unwrap();
        let cd = value_to_column_data(&Value::Date(d), &DataType::Date).unwrap();
        assert!(matches!(cd, ColumnData::Date(Some(_))));
    }

    #[test]
    fn timestamp_value() {
        let ts = Utc.with_ymd_and_hms(2025, 5, 13, 12, 0, 0).unwrap();
        let cd = value_to_column_data(&Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert!(matches!(cd, ColumnData::DateTime2(Some(_))));
    }

    #[test]
    fn uuid_value() {
        let u = uuid::Uuid::nil();
        let cd = value_to_column_data(&Value::Uuid(u), &DataType::Uuid).unwrap();
        match cd {
            ColumnData::Guid(Some(g)) => assert_eq!(g, u),
            _ => panic!("expected Guid"),
        }
    }

    #[test]
    fn json_value_as_string() {
        let cd = value_to_column_data(&Value::Json(serde_json::json!({"a": 1})), &DataType::Json)
            .unwrap();
        assert!(matches!(cd, ColumnData::String(Some(_))));
    }

    #[test]
    fn decimal_value() {
        let d = BigDecimal::from_str("123.45").unwrap();
        let cd = value_to_column_data(
            &Value::Decimal(d),
            &DataType::Decimal {
                precision: Some(10),
                scale: Some(2),
            },
        )
        .unwrap();
        match cd {
            ColumnData::Numeric(Some(n)) => {
                assert_eq!(n.scale(), 2);
                assert_eq!(n.value(), 12345i128);
            }
            _ => panic!("expected Numeric"),
        }
    }

    #[test]
    fn bigint_value() {
        let b = BigInt::from(42_i64);
        let cd =
            value_to_column_data(&Value::BigInt(b), &DataType::BigInt { width: Some(20) }).unwrap();
        match cd {
            ColumnData::Numeric(Some(n)) => {
                assert_eq!(n.scale(), 0);
                assert_eq!(n.value(), 42i128);
            }
            _ => panic!("expected Numeric"),
        }
    }

    #[test]
    fn null_typed() {
        let cd = value_to_column_data(&Value::Null, &DataType::Int32).unwrap();
        assert!(matches!(cd, ColumnData::I32(None)));
        let cd = value_to_column_data(&Value::Null, &DataType::Timestamp).unwrap();
        assert!(matches!(cd, ColumnData::DateTime2(None)));
    }

    #[test]
    fn image_custom_value() {
        let v = Value::Custom(Box::new(MssqlImageValue(vec![9, 8, 7])));
        let dt = DataType::Custom(Box::new(MssqlImageType));
        let cd = value_to_column_data(&v, &dt).unwrap();
        match cd {
            ColumnData::Binary(Some(b)) => assert_eq!(&*b, &[9, 8, 7]),
            _ => panic!("expected Binary"),
        }
    }

    #[test]
    fn rowversion_custom_errors() {
        let v = Value::Custom(Box::new(MssqlRowVersionValue(vec![0; 8])));
        let dt = DataType::Custom(Box::new(MssqlRowVersionType));
        let err = value_to_column_data(&v, &dt).unwrap_err();
        assert!(format!("{err}").contains("read-only"));
    }

    #[test]
    fn time_custom_value() {
        let t = NaiveTime::from_hms_opt(12, 34, 56).unwrap();
        let v = Value::Custom(Box::new(MssqlTimeValue(t)));
        let dt = DataType::Custom(Box::new(MssqlTimeType));
        let cd = value_to_column_data(&v, &dt).unwrap();
        assert!(matches!(cd, ColumnData::Time(Some(_))));
    }
}
