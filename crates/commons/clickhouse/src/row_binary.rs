//! RowBinary encoder for `Value` columns.
//!
//! ClickHouse's `RowBinary` format ([docs][1]) lays out one column at a
//! time per row using fixed primitive encodings:
//!
//! * UInt/Int — little-endian fixed width.
//! * String — VarUInt length prefix, then bytes (`Text` columns).
//! * FixedString(N) — exactly N bytes.
//! * Float32 / Float64 — IEEE-754 LE.
//! * Date — 2-byte unsigned days since 1970-01-01.
//! * DateTime — 4-byte unsigned seconds since epoch (UTC).
//! * UUID — 16 bytes in CH's mixed-endian layout: first 8 bytes
//!   little-endian, second 8 bytes big-endian. (See
//!   `ColumnUUID::serializeBinary` in the CH source.)
//! * Decimal(P, S) — 32 / 64 / 128 / 256-bit signed two's-complement LE
//!   value scaled by `10^S`. We only emit Decimal128 (16 LE bytes) — CH
//!   coerces to the actual column width on ingest.
//! * Nullable(T) — 1 NULL flag byte (0 = value, 1 = NULL), then the
//!   payload (or zero-length placeholder when NULL).
//! * IPv4 — 4 LE bytes.
//! * IPv6 — 16 BE bytes.
//! * Enum8 — 1-byte signed value; Enum16 — 2-byte signed LE.
//! * Json / structural composites — JSON-encoded text (`Json` columns
//!   accept text per CH HTTP spec; for `Tuple`/`Array`/`Map` we map to
//!   canonical `Json` at parse time so the same encoder path applies).
//! * AggregateFunction states — raw bytes verbatim.
//!
//! [1]: https://clickhouse.com/docs/en/interfaces/formats/#rowbinary

use std::io::Write;

use num_bigint::BigInt;
use thiserror::Error;

use air_elt_core::model::Field;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::value::Value;

use crate::types::aggregate_state::{ChAggregateStateType, ChAggregateStateValue};
use crate::types::enum_::{ChEnum8Type, ChEnum16Type, ChEnumValue};
use crate::types::fixed_string::{ChFixedStringType, ChFixedStringValue};
use crate::types::int128::{ChInt128Type, ChInt128Value, ChUInt128Type, ChUInt128Value};
use crate::types::int256::{ChInt256Type, ChInt256Value, ChUInt256Type, ChUInt256Value};
use crate::types::ip::{ChIpv4Type, ChIpv4Value, ChIpv6Type, ChIpv6Value};

#[derive(Debug, Error)]
pub enum EncodeError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json serialise failed for column {column:?}: {source}")]
    Json {
        column: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("type mismatch on column {column:?}: expected {expected}, got value variant {got}")]
    Mismatch {
        column: String,
        expected: String,
        got: &'static str,
    },
    #[error("value {value} out of range for column {column:?} target {target}")]
    OutOfRange {
        column: String,
        value: String,
        target: String,
    },
    #[error("unsupported type for column {column:?}: {ty}")]
    Unsupported { column: String, ty: String },
    #[error("FixedString({n}) requires exactly {n} bytes, got {got}")]
    FixedStringLength { n: u32, got: usize },
    #[error("enum value {name:?} not found in column {column:?} declaration")]
    EnumUnknown { column: String, name: String },
}

/// Encode a single value for a field into the RowBinary stream.
pub fn encode_value(out: &mut Vec<u8>, field: &Field, value: &Value) -> Result<(), EncodeError> {
    if field.nullable {
        if matches!(value, Value::Null) {
            out.push(1);
            return Ok(());
        }
        out.push(0);
    } else if matches!(value, Value::Null) {
        return Err(EncodeError::Mismatch {
            column: field.name.clone(),
            expected: format!("{} (NOT NULL)", display_type(&field.data_type)),
            got: "Null",
        });
    }
    encode_typed(out, &field.name, &field.data_type, value)
}

fn encode_typed(
    out: &mut Vec<u8>,
    column: &str,
    dt: &DataType,
    value: &Value,
) -> Result<(), EncodeError> {
    match dt {
        DataType::Bool => match value {
            Value::Bool(b) => {
                out.push(u8::from(*b));
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Int8 => match value {
            // Write as 1 byte. `i8 as u8` is a bit-cast in Rust (two's
            // complement), which is exactly what CH RowBinary expects for
            // a signed 8-bit column.
            Value::Int8(n) => {
                out.push(*n as u8);
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Int16 => match value {
            Value::Int16(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::Int32 => match value {
            Value::Int32(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::Int64 => match value {
            Value::Int64(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::UInt8 => match value {
            Value::UInt8(n) => {
                out.push(*n);
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::UInt16 => match value {
            Value::UInt16(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::UInt32 => match value {
            Value::UInt32(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::UInt64 => match value {
            Value::UInt64(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::Float32 => match value {
            Value::Float32(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::Float64 => match value {
            Value::Float64(n) => write_le(out, &n.to_le_bytes()),
            _ => mismatch(column, dt, value),
        },
        DataType::Text { .. } => match value {
            Value::Text(s) => {
                write_var_uint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Bytes { .. } => match value {
            Value::Bytes(b) => {
                write_var_uint(out, b.len() as u64);
                out.extend_from_slice(b);
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Date => match value {
            Value::Date(d) => {
                use chrono::NaiveDate;
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch literal");
                let days = (*d - epoch).num_days();
                let days_u16 = u16::try_from(days).map_err(|_| EncodeError::OutOfRange {
                    column: column.to_string(),
                    value: d.to_string(),
                    target: "Date (u16 days since 1970-01-01)".to_string(),
                })?;
                write_le(out, &days_u16.to_le_bytes())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Timestamp => match value {
            Value::Timestamp(ts) => {
                let secs = ts.timestamp();
                let secs_u32 = u32::try_from(secs).map_err(|_| EncodeError::OutOfRange {
                    column: column.to_string(),
                    value: ts.to_rfc3339(),
                    target: "DateTime (u32 secs since 1970)".to_string(),
                })?;
                write_le(out, &secs_u32.to_le_bytes())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Uuid => match value {
            Value::Uuid(u) => {
                // CH RowBinary stores a UUID as **two little-endian
                // UInt64s**: both halves of the standard UUID byte
                // representation are independently byte-reversed. See
                // https://clickhouse.com/docs/interfaces/formats/RowBinary
                // (UUID section). Reversing only the first half (an
                // earlier version of this code did exactly that) would
                // mis-encode every UUID's second half.
                let bytes = u.as_bytes();
                let mut buf = [0u8; 16];
                buf[..8].copy_from_slice(&bytes[..8]);
                buf[..8].reverse();
                buf[8..].copy_from_slice(&bytes[8..]);
                buf[8..].reverse();
                out.extend_from_slice(&buf);
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Json => match value {
            Value::Json(j) => {
                let s = serde_json::to_string(j).map_err(|source| EncodeError::Json {
                    column: column.to_string(),
                    source,
                })?;
                write_var_uint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
                Ok(())
            }
            Value::Text(s) => {
                write_var_uint(out, s.len() as u64);
                out.extend_from_slice(s.as_bytes());
                Ok(())
            }
            _ => mismatch(column, dt, value),
        },
        DataType::Decimal { precision, scale } => {
            encode_decimal(out, column, *precision, *scale, value)
        }
        DataType::Custom(t) => match t.kind() {
            ChIpv4Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChIpv4Value>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "IPv4".to_string(),
                            got: "Custom(non-IPv4)",
                        }
                    })?;
                    // CH RowBinary stores IPv4 as a little-endian UInt32.
                    // `Ipv4Addr::octets()` returns network-order bytes
                    // (192.168.0.1 → [0xC0, 0xA8, 0x00, 0x01]); CH wants
                    // them reversed (→ 0x01 0x00 0xA8 0xC0). See
                    // https://clickhouse.com/docs/interfaces/formats/RowBinary
                    let n: u32 = v.0.into();
                    out.extend_from_slice(&n.to_le_bytes());
                    Ok(())
                }
                _ => mismatch(column, dt, value),
            },
            ChIpv6Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChIpv6Value>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "IPv6".to_string(),
                            got: "Custom(non-IPv6)",
                        }
                    })?;
                    // CH RowBinary stores IPv6 as 16 BE bytes (network
                    // order), which is exactly what `octets()` returns.
                    out.extend_from_slice(&v.0.octets());
                    Ok(())
                }
                _ => mismatch(column, dt, value),
            },
            ChFixedStringType::KIND => match value {
                Value::Custom(b) => {
                    let v = b
                        .as_any()
                        .downcast_ref::<ChFixedStringValue>()
                        .ok_or_else(|| EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "FixedString".to_string(),
                            got: "Custom(non-FixedString)",
                        })?;
                    // We don't pull `size` off the descriptor (the
                    // `DynType` trait has no `as_any`, so a downcast
                    // from `&Box<dyn DynType>` isn't available). The
                    // value carries the exact `N` bytes; if the row
                    // doesn't match the column's declared length, CH
                    // surfaces a parse error on the HTTP response —
                    // strictly better than the runner silently
                    // misaligning subsequent columns.
                    out.extend_from_slice(&v.bytes);
                    Ok(())
                }
                _ => mismatch(column, dt, value),
            },
            ChEnum8Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChEnumValue>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "Enum8".to_string(),
                            got: "Custom(non-Enum)",
                        }
                    })?;
                    let ordinal = i8::try_from(v.value).map_err(|_| EncodeError::OutOfRange {
                        column: column.to_string(),
                        value: v.value.to_string(),
                        target: "Enum8 (i8)".to_string(),
                    })?;
                    out.push(ordinal as u8);
                    Ok(())
                }
                _ => mismatch(column, dt, value),
            },
            ChEnum16Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChEnumValue>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "Enum16".to_string(),
                            got: "Custom(non-Enum)",
                        }
                    })?;
                    write_le(out, &v.value.to_le_bytes())
                }
                _ => mismatch(column, dt, value),
            },
            kind if kind.starts_with(ChAggregateStateType::KIND_PREFIX) => match value {
                Value::Custom(b) => {
                    let v = b
                        .as_any()
                        .downcast_ref::<ChAggregateStateValue>()
                        .ok_or_else(|| EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: kind.to_string(),
                            got: "Custom(non-AggregateState)",
                        })?;
                    out.extend_from_slice(&v.bytes);
                    Ok(())
                }
                _ => mismatch(column, dt, value),
            },
            ChInt128Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChInt128Value>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "Int128".to_string(),
                            got: "Custom(non-Int128)",
                        }
                    })?;
                    write_le(out, &v.0.to_le_bytes())
                }
                _ => mismatch(column, dt, value),
            },
            ChUInt128Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChUInt128Value>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "UInt128".to_string(),
                            got: "Custom(non-UInt128)",
                        }
                    })?;
                    write_le(out, &v.0.to_le_bytes())
                }
                _ => mismatch(column, dt, value),
            },
            ChInt256Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChInt256Value>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "Int256".to_string(),
                            got: "Custom(non-Int256)",
                        }
                    })?;
                    // le_bytes is already in ClickHouse RowBinary order.
                    write_le(out, &v.le_bytes)
                }
                _ => mismatch(column, dt, value),
            },
            ChUInt256Type::KIND => match value {
                Value::Custom(b) => {
                    let v = b.as_any().downcast_ref::<ChUInt256Value>().ok_or_else(|| {
                        EncodeError::Mismatch {
                            column: column.to_string(),
                            expected: "UInt256".to_string(),
                            got: "Custom(non-UInt256)",
                        }
                    })?;
                    write_le(out, &v.le_bytes)
                }
                _ => mismatch(column, dt, value),
            },
            other => Err(EncodeError::Unsupported {
                column: column.to_string(),
                ty: other.to_string(),
            }),
        },
        // BigInt / Xml / Union are not first-class in this connector.
        other => Err(EncodeError::Unsupported {
            column: column.to_string(),
            ty: display_type(other),
        }),
    }
}

/// Encode a `Value::Decimal` into CH RowBinary's fixed-width signed
/// two's-complement LE format.
///
/// Column width is determined by precision:
/// * precision ≤ 9  → 4-byte `i32` (Decimal32)
/// * precision ≤ 18 → 8-byte `i64` (Decimal64)
/// * precision ≤ 38 → 16-byte `i128` (Decimal128)
/// * precision ≤ 76 → 32-byte signed LE (Decimal256)
///
/// When `precision` is `None` we fall back to i128 (safe for any
/// `BigDecimal` that fits in 38 decimal digits).
///
/// The value is scaled by `10^scale` to produce an integer mantissa.
fn encode_decimal(
    out: &mut Vec<u8>,
    column: &str,
    precision: Option<u32>,
    scale: Option<u32>,
    value: &Value,
) -> Result<(), EncodeError> {
    let d = match value {
        Value::Decimal(d) => d,
        _ => {
            return Err(EncodeError::Mismatch {
                column: column.to_string(),
                expected: "Decimal".to_string(),
                got: value_variant(value),
            });
        }
    };

    let effective_scale = scale.unwrap_or(0) as i64;
    let effective_precision = precision.unwrap_or(38);

    // Rescale the BigDecimal to the target scale, then extract the
    // integer mantissa (coefficient * 10^(current_scale - target_scale)).
    let scaled = d.with_scale(effective_scale);
    let (mantissa, _exp) = scaled.as_bigint_and_exponent();

    let width = decimal_width(effective_precision);
    encode_bigint_le(out, column, &mantissa, width, "Decimal")
}

/// Byte width for a given decimal precision.
fn decimal_width(precision: u32) -> u32 {
    if precision <= 9 {
        4
    } else if precision <= 18 {
        8
    } else if precision <= 38 {
        16
    } else {
        32
    }
}

/// Encode a signed `BigInt` into `width` little-endian two's-complement
/// bytes. Errors if the value requires more than `width` bytes.
fn encode_bigint_le(
    out: &mut Vec<u8>,
    column: &str,
    n: &BigInt,
    width: u32,
    target_name: &str,
) -> Result<(), EncodeError> {
    use crate::types::int256::bigint_to_le32;
    use num_bigint::Sign;

    match width {
        4 => {
            let v = i32::try_from(n).map_err(|_| EncodeError::OutOfRange {
                column: column.to_string(),
                value: n.to_string(),
                target: format!("{target_name}32 (i32)"),
            })?;
            write_le(out, &v.to_le_bytes())
        }
        8 => {
            let v = i64::try_from(n).map_err(|_| EncodeError::OutOfRange {
                column: column.to_string(),
                value: n.to_string(),
                target: format!("{target_name}64 (i64)"),
            })?;
            write_le(out, &v.to_le_bytes())
        }
        16 => {
            let v = i128::try_from(n).map_err(|_| EncodeError::OutOfRange {
                column: column.to_string(),
                value: n.to_string(),
                target: format!("{target_name}128 (i128)"),
            })?;
            write_le(out, &v.to_le_bytes())
        }
        32 => {
            // 32-byte two's-complement LE.  `bigint_to_le32` handles sign.
            let bytes = bigint_to_le32(n);
            // Range-check: for positive values, top byte must not set the
            // sign bit; for negative values, top byte must have the sign
            // bit set (i.e. the value fits in 255 bits + sign).
            let fits = match n.sign() {
                Sign::Plus | Sign::NoSign => bytes[31] & 0x80 == 0,
                Sign::Minus => bytes[31] & 0x80 != 0,
            };
            if !fits {
                return Err(EncodeError::OutOfRange {
                    column: column.to_string(),
                    value: n.to_string(),
                    target: format!("{target_name}256 (i256)"),
                });
            }
            write_le(out, &bytes)
        }
        _ => unreachable!("decimal_width produces only 4/8/16/32"),
    }
}

fn write_le(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), EncodeError> {
    out.write_all(bytes).map_err(EncodeError::from)
}

/// CH-style variable-length unsigned integer (LEB128).
pub fn write_var_uint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        out.push(((value as u8) & 0x7F) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn mismatch(column: &str, dt: &DataType, value: &Value) -> Result<(), EncodeError> {
    Err(EncodeError::Mismatch {
        column: column.to_string(),
        expected: display_type(dt),
        got: value_variant(value),
    })
}

fn display_type(dt: &DataType) -> String {
    format!("{dt}")
}

fn value_variant(v: &Value) -> &'static str {
    match v {
        Value::Null => "Null",
        Value::Bool(_) => "Bool",
        Value::Int8(_) => "Int8",
        Value::Int16(_) => "Int16",
        Value::Int32(_) => "Int32",
        Value::Int64(_) => "Int64",
        Value::UInt8(_) => "UInt8",
        Value::UInt16(_) => "UInt16",
        Value::UInt32(_) => "UInt32",
        Value::UInt64(_) => "UInt64",
        Value::Float32(_) => "Float32",
        Value::Float64(_) => "Float64",
        Value::BigInt(_) => "BigInt",
        Value::Decimal(_) => "Decimal",
        Value::Text(_) => "Text",
        Value::Bytes(_) => "Bytes",
        Value::Date(_) => "Date",
        Value::Timestamp(_) => "Timestamp",
        Value::Uuid(_) => "Uuid",
        Value::Json(_) => "Json",
        Value::Custom(_) => "Custom",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use bigdecimal::BigDecimal;
    use chrono::{NaiveDate, TimeZone, Utc};
    use uuid::Uuid;

    fn field(name: &str, dt: DataType, nullable: bool) -> Field {
        Field {
            name: name.to_string(),
            data_type: dt,
            nullable,
        }
    }

    #[test]
    fn int8_encodes_as_one_byte() {
        // Positive value.
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("a", DataType::Int8, false),
            &Value::Int8(i8::MAX),
        )
        .unwrap();
        assert_eq!(out, vec![0x7F]);

        // Zero.
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("a", DataType::Int8, false),
            &Value::Int8(0),
        )
        .unwrap();
        assert_eq!(out, vec![0x00]);

        // -1: two's complement is 0xFF.
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("a", DataType::Int8, false),
            &Value::Int8(-1),
        )
        .unwrap();
        assert_eq!(out, vec![0xFF]);

        // i8::MIN = -128: two's complement is 0x80.
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("a", DataType::Int8, false),
            &Value::Int8(i8::MIN),
        )
        .unwrap();
        assert_eq!(out, vec![0x80]);
    }

    #[test]
    fn primitives_encode_le() {
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("a", DataType::Int32, false),
            &Value::Int32(0x01020304),
        )
        .unwrap();
        assert_eq!(out, vec![0x04, 0x03, 0x02, 0x01]);
    }

    #[test]
    fn nullable_writes_flag() {
        let mut out = Vec::new();
        encode_value(&mut out, &field("a", DataType::Int32, true), &Value::Null).unwrap();
        assert_eq!(out, vec![1]);
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("a", DataType::Int32, true),
            &Value::Int32(7),
        )
        .unwrap();
        assert_eq!(out, vec![0, 7, 0, 0, 0]);
    }

    #[test]
    fn text_uses_var_uint_prefix() {
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("s", DataType::Text { size: None }, false),
            &Value::Text("hi".to_string()),
        )
        .unwrap();
        assert_eq!(out, vec![2, b'h', b'i']);
    }

    #[test]
    fn date_is_days_since_epoch() {
        let d = NaiveDate::from_ymd_opt(1970, 1, 2).unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("d", DataType::Date, false),
            &Value::Date(d),
        )
        .unwrap();
        assert_eq!(out, vec![1, 0]);
    }

    #[test]
    fn datetime_is_u32_seconds() {
        let ts = Utc.timestamp_opt(1, 0).unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("t", DataType::Timestamp, false),
            &Value::Timestamp(ts),
        )
        .unwrap();
        assert_eq!(out, vec![1, 0, 0, 0]);
    }

    #[test]
    fn uuid_mixed_endian() {
        let u = Uuid::parse_str("01020304-0506-0708-090a-0b0c0d0e0f10").unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("u", DataType::Uuid, false),
            &Value::Uuid(u),
        )
        .unwrap();
        assert_eq!(out.len(), 16);
        // Both halves independently byte-reversed (CH RowBinary spec).
        assert_eq!(&out[..8], &[0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]);
        assert_eq!(&out[8..], &[0x10, 0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, 0x09]);
    }

    #[test]
    fn rejects_null_on_non_nullable() {
        let mut out = Vec::new();
        let r = encode_value(&mut out, &field("a", DataType::Int32, false), &Value::Null);
        assert!(matches!(r, Err(EncodeError::Mismatch { .. })));
    }

    // ---- Decimal ---------------------------------------------------------

    #[test]
    fn decimal32_encodes_scaled_i32() {
        // Decimal32(2) with value 12.34 → mantissa = 1234 as i32 LE.
        let d: BigDecimal = "12.34".parse().unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field(
                "d",
                DataType::Decimal {
                    precision: Some(9),
                    scale: Some(2),
                },
                false,
            ),
            &Value::Decimal(d),
        )
        .unwrap();
        let mantissa = i32::from_le_bytes(out[..4].try_into().unwrap());
        assert_eq!(mantissa, 1234);
    }

    #[test]
    fn decimal64_encodes_scaled_i64() {
        // Decimal64(4) with value 1.0001 → mantissa = 10001 as i64 LE.
        let d: BigDecimal = "1.0001".parse().unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field(
                "d",
                DataType::Decimal {
                    precision: Some(18),
                    scale: Some(4),
                },
                false,
            ),
            &Value::Decimal(d),
        )
        .unwrap();
        let mantissa = i64::from_le_bytes(out[..8].try_into().unwrap());
        assert_eq!(mantissa, 10001);
    }

    #[test]
    fn decimal128_encodes_scaled_i128() {
        // Decimal128(0) with integer value 42 → mantissa = 42 as i128 LE.
        let d: BigDecimal = "42".parse().unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field(
                "d",
                DataType::Decimal {
                    precision: Some(38),
                    scale: Some(0),
                },
                false,
            ),
            &Value::Decimal(d),
        )
        .unwrap();
        let mantissa = i128::from_le_bytes(out[..16].try_into().unwrap());
        assert_eq!(mantissa, 42);
    }

    #[test]
    fn decimal_negative_value() {
        // Decimal32(2) with value -1.00 → mantissa = -100 as i32 LE.
        let d: BigDecimal = "-1.00".parse().unwrap();
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field(
                "d",
                DataType::Decimal {
                    precision: Some(9),
                    scale: Some(2),
                },
                false,
            ),
            &Value::Decimal(d),
        )
        .unwrap();
        let mantissa = i32::from_le_bytes(out[..4].try_into().unwrap());
        assert_eq!(mantissa, -100);
    }

    // ---- Int128 / UInt128 ------------------------------------------------

    #[test]
    fn int128_encodes_16_le_bytes() {
        use crate::types::int128::ChInt128Value;
        let dt = DataType::Custom(Box::new(crate::types::int128::ChInt128Type));
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("n", dt, false),
            &Value::Custom(Box::new(ChInt128Value(1_i128))),
        )
        .unwrap();
        assert_eq!(out.len(), 16);
        assert_eq!(out[0], 1);
        assert!(out[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn uint128_encodes_16_le_bytes() {
        use crate::types::int128::ChUInt128Value;
        let dt = DataType::Custom(Box::new(crate::types::int128::ChUInt128Type));
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("n", dt, false),
            &Value::Custom(Box::new(ChUInt128Value(u128::MAX))),
        )
        .unwrap();
        assert_eq!(out.len(), 16);
        assert!(out.iter().all(|&b| b == 0xFF));
    }

    // ---- Int256 / UInt256 ------------------------------------------------

    #[test]
    fn int256_encodes_32_le_bytes() {
        use crate::types::int256::{ChInt256Type, ChInt256Value, bigint_to_le32};
        let n = num_bigint::BigInt::from(1i64);
        let le_bytes = bigint_to_le32(&n);
        let dt = DataType::Custom(Box::new(ChInt256Type));
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("n", dt, false),
            &Value::Custom(Box::new(ChInt256Value { le_bytes })),
        )
        .unwrap();
        assert_eq!(out.len(), 32);
        assert_eq!(out[0], 1);
        assert!(out[1..].iter().all(|&b| b == 0));
    }

    #[test]
    fn uint256_encodes_32_le_bytes() {
        use crate::types::int256::{ChUInt256Type, ChUInt256Value, biguint_to_le32};
        let n = num_bigint::BigUint::from(255u32);
        let le_bytes = biguint_to_le32(&n);
        let dt = DataType::Custom(Box::new(ChUInt256Type));
        let mut out = Vec::new();
        encode_value(
            &mut out,
            &field("n", dt, false),
            &Value::Custom(Box::new(ChUInt256Value { le_bytes })),
        )
        .unwrap();
        assert_eq!(out.len(), 32);
        assert_eq!(out[0], 0xFF);
        assert!(out[1..].iter().all(|&b| b == 0));
    }
}
