//! `Timestamp → Date` under `truncate=true`. Drops the time-of-day part of
//! the UTC timestamp.

use super::error::ConvertError;
use crate::types::{DataType, Value};

pub fn convert(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    match value {
        Value::Timestamp(ts) => Ok(Value::Date(ts.date_naive())),
        _ => Err(ConvertError::ValueShapeMismatch { src: *src }),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use chrono::{DateTime, NaiveDate, Utc};

    use super::*;

    #[test]
    fn value_shape_mismatch() {
        let d = NaiveDate::from_ymd_opt(2024, 5, 17).unwrap();
        let res = convert(Value::Date(d), &DataType::Timestamp);
        assert!(matches!(res, Err(ConvertError::ValueShapeMismatch { .. })));
    }

    #[test]
    fn extracts_utc_date_part() {
        let ts: DateTime<Utc> = "2024-05-17T12:34:56Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(2024, 5, 17).unwrap())
        );
    }

    #[test]
    fn midnight_utc_keeps_same_date() {
        let ts: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
    }

    #[test]
    fn late_evening_utc_keeps_same_date() {
        let ts: DateTime<Utc> = "2024-12-31T23:59:59Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap())
        );
    }
}
