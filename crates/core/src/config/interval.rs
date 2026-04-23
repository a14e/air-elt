use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("invalid interval {raw:?}: {reason}")]
pub struct IntervalError {
    raw: String,
    reason: String,
}

pub fn parse(raw: &str) -> Result<Duration, IntervalError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(err(raw, "empty string"));
    }
    if trimmed.starts_with('P') || trimmed.starts_with('p') {
        parse_iso(raw, trimmed)
    } else {
        parse_human(raw, trimmed)
    }
}

fn parse_iso(raw: &str, s: &str) -> Result<Duration, IntervalError> {
    let upper = s.to_ascii_uppercase();
    let dur = iso8601_duration::Duration::parse(&upper)
        .map_err(|_| err(raw, "invalid ISO 8601 duration"))?;

    if dur.year > 0.0 || dur.month > 0.0 {
        return Err(err(
            raw,
            "ISO durations with years/months are not supported",
        ));
    }

    let total_secs = f64::from(dur.day) * 86_400.0
        + f64::from(dur.hour) * 3_600.0
        + f64::from(dur.minute) * 60.0
        + f64::from(dur.second);

    if total_secs <= 0.0 {
        return Err(err(raw, "duration is zero"));
    }
    if !total_secs.is_finite() || total_secs > u64::MAX as f64 {
        return Err(err(raw, "overflow"));
    }

    Ok(Duration::from_secs_f64(total_secs))
}

const UNIT_ORDER_D: u8 = 5;
const UNIT_ORDER_H: u8 = 4;
const UNIT_ORDER_M: u8 = 3;
const UNIT_ORDER_S: u8 = 2;
const UNIT_ORDER_MS: u8 = 1;
const UNIT_ORDER_W: u8 = 6;

fn parse_human(raw: &str, s: &str) -> Result<Duration, IntervalError> {
    let mut total = Duration::ZERO;
    let mut pos = 0;
    let bytes = s.as_bytes();
    let mut last_order: u8 = u8::MAX;

    while pos < bytes.len() {
        while pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }
        if pos >= bytes.len() {
            break;
        }

        let num_start = pos;
        while pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b'.') {
            pos += 1;
        }
        if pos == num_start {
            return Err(err(raw, &format!("expected number at position {pos}")));
        }
        let num_str = &s[num_start..pos];

        while pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }

        let unit_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_alphabetic() {
            pos += 1;
        }
        let unit = &s[unit_start..pos];

        let (order, dur) = match unit.to_ascii_lowercase().as_str() {
            "ms" | "millis" | "millisecond" | "milliseconds" => {
                let n = parse_f64(raw, num_str)?;
                (UNIT_ORDER_MS, Duration::from_secs_f64(n / 1000.0))
            }
            "s" | "sec" | "second" | "seconds" | "" => {
                let n = parse_f64(raw, num_str)?;
                (UNIT_ORDER_S, Duration::from_secs_f64(n))
            }
            "m" | "min" | "minute" | "minutes" => {
                let n = parse_u64(raw, num_str)?;
                (UNIT_ORDER_M, Duration::from_secs(checked(raw, n, 60)?))
            }
            "h" | "hr" | "hour" | "hours" => {
                let n = parse_u64(raw, num_str)?;
                (UNIT_ORDER_H, Duration::from_secs(checked(raw, n, 3600)?))
            }
            "d" | "day" | "days" => {
                let n = parse_u64(raw, num_str)?;
                (UNIT_ORDER_D, Duration::from_secs(checked(raw, n, 86_400)?))
            }
            "w" | "wk" | "week" | "weeks" => {
                let n = parse_u64(raw, num_str)?;
                (UNIT_ORDER_W, Duration::from_secs(checked(raw, n, 604_800)?))
            }
            "y" | "yr" | "year" | "years" => {
                return Err(err(raw, "years are not supported (ambiguous length)"));
            }
            "mo" | "month" | "months" => {
                return Err(err(raw, "months are not supported (ambiguous length)"));
            }
            other => return Err(err(raw, &format!("unknown unit {other:?}"))),
        };

        if order >= last_order {
            return Err(err(raw, "units must be in decreasing order"));
        }
        last_order = order;
        total += dur;
    }

    if total.is_zero() {
        return Err(err(raw, "duration is zero"));
    }
    Ok(total)
}

fn parse_u64(raw: &str, s: &str) -> Result<u64, IntervalError> {
    s.parse::<u64>()
        .map_err(|_| err(raw, &format!("cannot parse integer {s:?}")))
}

fn parse_f64(raw: &str, s: &str) -> Result<f64, IntervalError> {
    s.parse::<f64>()
        .map_err(|_| err(raw, &format!("cannot parse number {s:?}")))
}

fn checked(raw: &str, n: u64, mult: u64) -> Result<u64, IntervalError> {
    n.checked_mul(mult).ok_or_else(|| err(raw, "overflow"))
}

fn err(raw: &str, reason: &str) -> IntervalError {
    IntervalError {
        raw: raw.to_string(),
        reason: reason.to_string(),
    }
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    parse(&s).map_err(serde::de::Error::custom)
}

pub fn to_iso(duration: &Duration) -> String {
    let total_secs = duration.as_secs();
    let ms = duration.subsec_millis();
    if total_secs == 0 && ms > 0 {
        return format!("PT0.{ms:03}S");
    }
    let days = total_secs / 86_400;
    let remaining = total_secs % 86_400;
    let hours = remaining / 3600;
    let remaining = remaining % 3600;
    let minutes = remaining / 60;
    let secs = remaining % 60;

    let mut s = String::from("P");
    if days > 0 {
        s.push_str(&format!("{days}D"));
    }
    if hours > 0 || minutes > 0 || secs > 0 || ms > 0 {
        s.push('T');
        if hours > 0 {
            s.push_str(&format!("{hours}H"));
        }
        if minutes > 0 {
            s.push_str(&format!("{minutes}M"));
        }
        if ms > 0 {
            s.push_str(&format!("{secs}.{ms:03}S"));
        } else if secs > 0 {
            s.push_str(&format!("{secs}S"));
        }
    }
    s
}

pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    to_iso(duration).serialize(serializer)
}

pub fn deserialize_opt<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt: Option<String> = Option::deserialize(deserializer)?;
    match opt {
        Some(s) => parse(&s).map(Some).map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}

pub fn serialize_opt<S>(duration: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match duration {
        Some(d) => serialize(d, serializer),
        None => serializer.serialize_none(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn short_units() {
        assert_eq!(parse("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse("1s").unwrap(), Duration::from_secs(1));
        assert_eq!(parse("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse("1h").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse("3d").unwrap(), Duration::from_secs(3 * 86400));
        assert_eq!(parse("1w").unwrap(), Duration::from_secs(604_800));
    }

    #[test]
    fn bare_number_is_seconds() {
        assert_eq!(parse("42").unwrap(), Duration::from_secs(42));
    }

    #[test]
    fn long_units() {
        assert_eq!(parse("1 second").unwrap(), Duration::from_secs(1));
        assert_eq!(parse("5 seconds").unwrap(), Duration::from_secs(5));
        assert_eq!(parse("2 minutes").unwrap(), Duration::from_secs(120));
        assert_eq!(parse("1 hour").unwrap(), Duration::from_secs(3600));
        assert_eq!(parse("1 day").unwrap(), Duration::from_secs(86400));
        assert_eq!(parse("1 week").unwrap(), Duration::from_secs(604_800));
        assert_eq!(
            parse("100 milliseconds").unwrap(),
            Duration::from_millis(100)
        );
    }

    #[test]
    fn compound() {
        assert_eq!(parse("1h30m").unwrap(), Duration::from_secs(5400));
        assert_eq!(parse("1h5s").unwrap(), Duration::from_secs(3605));
        assert_eq!(
            parse("2d3h5m10s").unwrap(),
            Duration::from_secs(2 * 86400 + 3 * 3600 + 5 * 60 + 10)
        );
        assert_eq!(parse("1m500ms").unwrap(), Duration::from_millis(60_500));
        assert_eq!(parse("1w2d").unwrap(), Duration::from_secs(9 * 86400));
    }

    #[test]
    fn fractional_seconds() {
        assert_eq!(parse("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse("0.5s").unwrap(), Duration::from_millis(500));
        assert_eq!(parse("1s500ms").unwrap(), Duration::from_millis(1500));
    }

    #[test]
    fn iso_8601() {
        assert_eq!(parse("PT1H5S").unwrap(), Duration::from_secs(3605));
        assert_eq!(parse("PT30M").unwrap(), Duration::from_secs(1800));
        assert_eq!(parse("P1DT2H").unwrap(), Duration::from_secs(86400 + 7200));
        assert_eq!(parse("PT500S").unwrap(), Duration::from_secs(500));
        assert_eq!(parse("P2D").unwrap(), Duration::from_secs(2 * 86400));
        assert_eq!(parse("P1W").unwrap(), Duration::from_secs(7 * 86400));
        assert_eq!(
            parse("P1DT2H30M").unwrap(),
            Duration::from_secs(86400 + 7200 + 1800)
        );
    }

    #[test]
    fn iso_fractional_seconds() {
        assert_eq!(parse("PT1.5S").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse("PT0.5S").unwrap(), Duration::from_millis(500));
    }

    #[test]
    fn iso_case_insensitive() {
        assert_eq!(parse("pt1h5s").unwrap(), Duration::from_secs(3605));
        assert_eq!(parse("p1dt2h").unwrap(), Duration::from_secs(93600));
    }

    #[test]
    fn whitespace_trimmed() {
        assert_eq!(parse("  1h  ").unwrap(), Duration::from_secs(3600));
    }

    #[test]
    fn wrong_order_rejected() {
        assert!(parse("1m1h").is_err());
        assert!(parse("1s1m").is_err());
        assert!(parse("1ms1s").is_err());
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("abc").is_err());
        assert!(parse("5xyz").is_err());
        assert!(parse("0s").is_err());
        assert!(parse("PT").is_err());
        assert!(parse("P1Y").is_err());
        assert!(parse("P1M").is_err());
        assert!(parse("1y").is_err());
        assert!(parse("1 year").is_err());
        assert!(parse("2 months").is_err());
        assert!(parse("1mo").is_err());
    }

    #[test]
    fn overflow_rejected() {
        assert!(parse("999999999999999999d").is_err());
        assert!(parse("PT999999999999999999H").is_err());
    }

    #[test]
    fn serde_roundtrip() {
        #[derive(Serialize, Deserialize, Debug, PartialEq)]
        struct Cfg {
            #[serde(
                serialize_with = "super::serialize",
                deserialize_with = "super::deserialize"
            )]
            interval: Duration,
        }

        let cfg = Cfg {
            interval: Duration::from_secs(3600),
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        assert!(toml_str.contains("PT1H"));
        let parsed: Cfg = toml::from_str(&toml_str).unwrap();
        assert_eq!(parsed, cfg);

        let cfg2 = Cfg {
            interval: Duration::from_millis(1500),
        };
        let toml_str2 = toml::to_string(&cfg2).unwrap();
        assert!(toml_str2.contains("PT1.500S"));
        let parsed2: Cfg = toml::from_str(&toml_str2).unwrap();
        assert_eq!(parsed2, cfg2);

        let cfg3 = Cfg {
            interval: Duration::from_secs(604_800),
        };
        let toml_str3 = toml::to_string(&cfg3).unwrap();
        assert!(toml_str3.contains("P7D"));
    }

    #[test]
    fn to_iso_format() {
        assert_eq!(to_iso(&Duration::from_secs(1)), "PT1S");
        assert_eq!(to_iso(&Duration::from_secs(60)), "PT1M");
        assert_eq!(to_iso(&Duration::from_secs(3600)), "PT1H");
        assert_eq!(to_iso(&Duration::from_secs(86400)), "P1D");
        assert_eq!(to_iso(&Duration::from_secs(90061)), "P1DT1H1M1S");
        assert_eq!(to_iso(&Duration::from_millis(500)), "PT0.500S");
        assert_eq!(to_iso(&Duration::from_millis(1500)), "PT1.500S");
    }
}
