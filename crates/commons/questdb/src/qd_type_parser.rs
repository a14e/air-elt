//! Parser for QuestDB native type names as they appear in the second
//! column of `SHOW COLUMNS FROM "<table>"`.
//!
//! QuestDB types have a small, mostly flat surface:
//!
//! * Primitives — `BOOLEAN`, `BYTE`, `SHORT`, `CHAR`, `INT`, `LONG`,
//!   `FLOAT`, `DOUBLE`, `STRING`, `VARCHAR`, `DATE`, `TIMESTAMP`, `UUID`,
//!   `BINARY`.
//! * `SYMBOL` — may carry decorators (`CAPACITY 128`, `NOCACHE`, `INDEX`,
//!   `INDEX CAPACITY 256`). The decorators do not change the canonical
//!   pivot, so we accept them and discard.
//! * `LONG256` — opaque 256-bit value carrier.
//! * `IPv4` (case-insensitive) — dotted-quad address carrier.
//! * `GEOHASH(Nb)` / `GEOHASH(Nc)` — `N` bits of dictionary-encoded
//!   geohash. Both width units accepted: a `c` (character) width converts
//!   into bits by `bits = chars * 5`. Bit range enforced at `1..=60`.
//!
//! Unknown types are rejected with [`ParseError::Unknown`] so the sink
//! can surface a clear "unsupported QuestDB type" error to the operator.

use thiserror::Error;

use air_elt_core::types::data_type::DataType;

use crate::types::geohash::QuestDbGeohashType;
use crate::types::long256::QuestDbLong256Type;
use crate::types::symbol::QuestDbSymbolType;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("empty type string")]
    Empty,
    #[error("unknown QuestDB type {native:?}")]
    Unknown { native: String },
    #[error("malformed type {native:?}: {reason}")]
    Malformed { native: String, reason: String },
}

/// Parse a QuestDB native type name into a canonical [`DataType`].
///
/// Nullability is **not** carried in the QuestDB native type string —
/// every column is nullable except the designated timestamp, whose
/// non-nullability is enforced separately during schema folding. Callers
/// (the schema introspector) override the nullability flag accordingly.
pub fn parse_type(input: &str) -> Result<DataType, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }

    let upper = trimmed.to_ascii_uppercase();

    // `IPv4` is the one case where QuestDB itself surfaces a mixed-case
    // type name. Normalise to upper-case before matching the discriminator
    // and keep the original `trimmed` for error reporting.
    let head = upper.as_str();

    // SYMBOL with optional decorators: accept everything starting with
    // `SYMBOL` followed by whitespace or end-of-string.
    if head == "SYMBOL" || head.starts_with("SYMBOL ") {
        return Ok(DataType::Custom(Box::new(QuestDbSymbolType)));
    }

    // GEOHASH(Nb) / GEOHASH(Nc) — parse the bit width.
    if let Some(rest) = head.strip_prefix("GEOHASH(") {
        let rest = rest
            .strip_suffix(')')
            .ok_or_else(|| ParseError::Malformed {
                native: trimmed.to_string(),
                reason: "missing closing ')'".to_string(),
            })?;
        let bits = parse_geohash_width(rest).map_err(|reason| ParseError::Malformed {
            native: trimmed.to_string(),
            reason,
        })?;
        return Ok(DataType::Custom(Box::new(QuestDbGeohashType { bits })));
    }

    let data_type = match head {
        "BOOLEAN" => DataType::Bool,
        "BYTE" => DataType::Int8,
        "SHORT" => DataType::Int16,
        // `CHAR` is a single UTF-16 codepoint in QuestDB; surface as
        // `Text { size: Some(1) }` so the matrix knows the width.
        "CHAR" => DataType::Text { size: Some(1) },
        "INT" => DataType::Int32,
        "LONG" => DataType::Int64,
        "FLOAT" => DataType::Float32,
        "DOUBLE" => DataType::Float64,
        // `STRING` and `VARCHAR` differ in storage layout server-side but
        // both surface as unsized text on the canonical pivot.
        "STRING" | "VARCHAR" => DataType::Text { size: None },
        // `DATE` in QuestDB is a millisecond-precision wall time; the
        // canonical `Date` pivot is calendar-only (NaiveDate). The pg-wire
        // binder coerces a `Value::Date` into the millisecond epoch
        // (start-of-day UTC) — sufficient when the source is a `Date`.
        // Operators writing sub-day-precision values into a QuestDB DATE
        // column should declare the source column as `Timestamp` instead.
        "DATE" => DataType::Date,
        "TIMESTAMP" => DataType::Timestamp,
        "UUID" => DataType::Uuid,
        "BINARY" => DataType::Bytes { size: None },
        "LONG256" => DataType::Custom(Box::new(QuestDbLong256Type)),
        "IPV4" => DataType::Ipv4,
        _ => {
            return Err(ParseError::Unknown {
                native: trimmed.to_string(),
            });
        }
    };

    Ok(data_type)
}

fn parse_geohash_width(spec: &str) -> Result<u8, String> {
    let spec = spec.trim();
    if spec.len() < 2 {
        return Err(format!("expected `<N>b` or `<N>c`, got {spec:?}"));
    }
    let last = spec
        .chars()
        .next_back()
        .ok_or_else(|| "empty width".to_string())?;
    let digits = &spec[..spec.len() - last.len_utf8()];
    let n: u32 = digits
        .trim()
        .parse()
        .map_err(|e: std::num::ParseIntError| format!("invalid number {digits:?}: {e}"))?;
    let bits = match last {
        'B' | 'b' => n,
        'C' | 'c' => n.checked_mul(5).ok_or_else(|| "overflow".to_string())?,
        other => return Err(format!("unexpected width unit {other:?}")),
    };
    if !(1..=60).contains(&bits) {
        return Err(format!("bit width {bits} outside 1..=60"));
    }
    Ok(bits as u8)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn parse(s: &str) -> DataType {
        parse_type(s).unwrap_or_else(|e| panic!("failed to parse {s:?}: {e}"))
    }

    #[test]
    fn boolean_to_bool() {
        assert_eq!(parse("BOOLEAN"), DataType::Bool);
    }

    #[test]
    fn byte_to_int8() {
        assert_eq!(parse("BYTE"), DataType::Int8);
    }

    #[test]
    fn short_to_int16() {
        assert_eq!(parse("SHORT"), DataType::Int16);
    }

    #[test]
    fn char_to_text_size_one() {
        assert_eq!(parse("CHAR"), DataType::Text { size: Some(1) });
    }

    #[test]
    fn int_to_int32() {
        assert_eq!(parse("INT"), DataType::Int32);
    }

    #[test]
    fn long_to_int64() {
        assert_eq!(parse("LONG"), DataType::Int64);
    }

    #[test]
    fn float_to_float32() {
        assert_eq!(parse("FLOAT"), DataType::Float32);
    }

    #[test]
    fn double_to_float64() {
        assert_eq!(parse("DOUBLE"), DataType::Float64);
    }

    #[test]
    fn string_to_text() {
        assert_eq!(parse("STRING"), DataType::Text { size: None });
    }

    #[test]
    fn varchar_to_text() {
        assert_eq!(parse("VARCHAR"), DataType::Text { size: None });
    }

    #[test]
    fn date_to_date() {
        assert_eq!(parse("DATE"), DataType::Date);
    }

    #[test]
    fn timestamp_to_timestamp() {
        assert_eq!(parse("TIMESTAMP"), DataType::Timestamp);
    }

    #[test]
    fn uuid_to_uuid() {
        assert_eq!(parse("UUID"), DataType::Uuid);
    }

    #[test]
    fn binary_to_bytes() {
        assert_eq!(parse("BINARY"), DataType::Bytes { size: None });
    }

    #[test]
    fn symbol_plain() {
        let p = parse("SYMBOL");
        match &p {
            DataType::Custom(t) => assert_eq!(t.kind(), "questdb.symbol"),
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn symbol_with_decorators() {
        let p = parse("SYMBOL CAPACITY 128 NOCACHE INDEX CAPACITY 256");
        match &p {
            DataType::Custom(t) => assert_eq!(t.kind(), "questdb.symbol"),
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn long256_to_custom() {
        let p = parse("LONG256");
        match &p {
            DataType::Custom(t) => assert_eq!(t.kind(), "questdb.long256"),
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn ipv4_case_insensitive() {
        assert_eq!(parse("IPv4"), DataType::Ipv4);
        assert_eq!(parse("IPV4"), DataType::Ipv4);
    }

    #[test]
    fn geohash_bits_form() {
        let p = parse("GEOHASH(7b)");
        match &p {
            DataType::Custom(t) => {
                assert_eq!(t.kind(), "questdb.geohash");
                let g = t
                    .as_any()
                    .downcast_ref::<QuestDbGeohashType>()
                    .expect("geohash");
                assert_eq!(g.bits, 7);
            }
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn geohash_chars_form() {
        // 2 chars = 10 bits.
        let p = parse("GEOHASH(2c)");
        match &p {
            DataType::Custom(t) => {
                let g = t
                    .as_any()
                    .downcast_ref::<QuestDbGeohashType>()
                    .expect("geohash");
                assert_eq!(g.bits, 10);
            }
            _ => panic!("expected Custom"),
        }
    }

    #[test]
    fn geohash_bit_width_out_of_range_rejected() {
        // 0 bits and >60 bits are both invalid.
        assert!(matches!(
            parse_type("GEOHASH(0b)"),
            Err(ParseError::Malformed { .. })
        ));
        assert!(matches!(
            parse_type("GEOHASH(61b)"),
            Err(ParseError::Malformed { .. })
        ));
    }

    #[test]
    fn unknown_rejected() {
        let err = parse_type("FOO").unwrap_err();
        assert!(matches!(err, ParseError::Unknown { ref native } if native == "FOO"));
    }

    #[test]
    fn empty_rejected() {
        assert!(matches!(parse_type(""), Err(ParseError::Empty)));
        assert!(matches!(parse_type("   "), Err(ParseError::Empty)));
    }
}
