//! `Timestamp → Date` under `truncate=true`. Drops the time-of-day part of
//! the UTC timestamp.

use super::error::ConvertError;
use crate::{DataType, Value};

pub fn convert(value: Value, src: &DataType) -> Result<Value, ConvertError> {
    match value {
        Value::Timestamp(ts) => Ok(Value::Date(ts.date_naive())),
        _ => Err(ConvertError::ValueShapeMismatch { src: src.clone() }),
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
    fn midnight_utc_keeps_same_date() {
        // Edge: the lowest time-of-day in a UTC day. The property
        // `timestamp_to_date_matches_iso_date_string` covers the
        // general claim; this anchor pins the midnight boundary.
        let ts: DateTime<Utc> = "2024-01-01T00:00:00Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap())
        );
    }

    #[test]
    fn late_evening_utc_keeps_same_date() {
        // Edge: the highest second-resolution time-of-day in a UTC day.
        let ts: DateTime<Utc> = "2024-12-31T23:59:59Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap())
        );
    }

    #[test]
    fn timestamp_to_date_epoch() {
        // Explicit anchor on the unix epoch — the well-known origin of
        // every timestamp implementation in the project.
        let ts: DateTime<Utc> = "1970-01-01T00:00:00Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap())
        );
    }

    #[test]
    fn timestamp_to_date_leap_year_feb29() {
        // Edge: the leap-day instant. A naive implementation that
        // bypassed `date_naive()` and rolled its own year/month/day
        // split would trip here.
        let ts: DateTime<Utc> = "2024-02-29T23:59:59Z".parse().unwrap();
        let out = convert(Value::Timestamp(ts), &DataType::Timestamp).unwrap();
        assert_eq!(
            out,
            Value::Date(NaiveDate::from_ymd_opt(2024, 2, 29).unwrap())
        );
    }

    // ---- Property-based tests --------------------------------------

    use proptest::prelude::*;

    /// Yields any UTC timestamp inside the representable range that
    /// `chrono::DateTime::from_timestamp` accepts.
    fn any_utc_timestamp() -> impl Strategy<Value = DateTime<Utc>> {
        any::<i64>().prop_filter_map("representable", |seconds| {
            // Clamp into a safe range around the unix epoch (year ~1900–2200).
            let bounded = seconds.rem_euclid(8_000_000_000) - 4_000_000_000;
            DateTime::<Utc>::from_timestamp(bounded, 0)
        })
    }

    /// Independent oracle: format the UTC timestamp as an ISO date
    /// string and parse it back through `NaiveDate::parse_from_str`.
    /// That goes through a different chrono code path than the
    /// `date_naive()` shortcut used by the production conversion,
    /// so a regression that breaks one but not the other surfaces here.
    #[test_strategy::proptest(ProptestConfig::with_cases(256))]
    fn timestamp_to_date_matches_iso_date_string(
        #[strategy(any_utc_timestamp())] ts: DateTime<Utc>,
    ) {
        let iso = ts.format("%Y-%m-%d").to_string();
        let expected =
            chrono::NaiveDate::parse_from_str(&iso, "%Y-%m-%d").expect("iso parses back");
        let got = convert(Value::Timestamp(ts), &DataType::Timestamp).expect("convert");
        prop_assert_eq!(got, Value::Date(expected));
    }
}
