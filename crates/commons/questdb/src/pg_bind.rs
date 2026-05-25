//! Sink-side `Separated` binding for QuestDB pg-wire writes.
//!
//! QuestDB's pg-wire surface is a subset of Postgres' — `sqlx::Postgres`
//! drives the connection but the column types differ. We bind everything
//! via sqlx's standard type plumbing; custom QuestDB types (`SYMBOL`,
//! `LONG256`, `IPv4`, `GEOHASH`) all go on the wire as TEXT — QuestDB
//! coerces server-side on INSERT.
//!
//! The shape mirrors `air-elt-commons-mysql::sink_bind` (which itself
//! mirrors `commons-pg::sink_bind`); we intentionally do not depend on
//! either pg-flavoured crate.

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Postgres;
use sqlx::query_builder::Separated;
use thiserror::Error;
use uuid::Uuid;

use air_elt_core::error::{RuntimeError, TypeError};
use air_elt_core::model::Field;
use air_elt_core::types::data_type::DataType;
use air_elt_core::types::value::Value;

use crate::types::geohash::QuestDbGeohashValue;
use crate::types::is_questdb_native_kind;
use crate::types::long256::QuestDbLong256Value;
use crate::types::symbol::QuestDbSymbolValue;

#[derive(Debug, Error)]
pub enum BindError {
    #[error(
        "column {column:?}: type {got_kind:?} is not supported by the QuestDB pg-wire sink for canonical {expected}"
    )]
    UnsupportedType {
        column: String,
        expected: String,
        got_kind: String,
    },
    #[error("column {column:?}: json encode failed: {source}")]
    Json {
        column: String,
        #[source]
        source: serde_json::Error,
    },
}

impl From<BindError> for RuntimeError {
    /// Map `BindError` into a typed `RuntimeError` so the runner can tell
    /// type-shape mistakes apart from backend-driver faults. `RuntimeError::Backend`
    /// triggers the runner's ctx-drop / reconnect path; the variants below
    /// do not — the connection is fine, the row is not.
    fn from(value: BindError) -> Self {
        match value {
            BindError::UnsupportedType {
                column,
                expected,
                got_kind,
            } => RuntimeError::Type(TypeError::SinkValueUnsupported {
                column,
                expected,
                got_kind,
            }),
            BindError::Json { source, .. } => {
                // Preserve the underlying serde_json error chain directly.
                RuntimeError::Serde(source)
            }
        }
    }
}

/// Bind one `(Field, Value)` cell into a sqlx `Separated` chain. Mirrors
/// `commons-mysql::sink_bind::bind_value_separated` in shape — the only
/// QuestDB-specific arms are the custom types (`SYMBOL` / `LONG256` /
/// `IPv4` / `GEOHASH`) and `Json` (rendered through
/// [`value_to_json`] so cursor-storage envelopes never leak).
pub fn bind_value_separated_pg(
    chain: &mut Separated<'_, '_, Postgres, &str>,
    field: &Field,
    value: &Value,
) -> Result<(), BindError> {
    let dt = &field.data_type;
    let column = field.name.as_str();
    match value {
        Value::Null => bind_null(chain, field),
        Value::Bool(b) => {
            chain.push_bind(*b);
            Ok(())
        }
        Value::Int8(n) => {
            // Postgres has no int1 — widen to int2 for the wire (QuestDB
            // BYTE accepts the implicit cast).
            chain.push_bind(i16::from(*n));
            Ok(())
        }
        Value::Int16(n) => {
            chain.push_bind(*n);
            Ok(())
        }
        Value::Int32(n) => {
            chain.push_bind(*n);
            Ok(())
        }
        Value::Int64(n) => {
            chain.push_bind(*n);
            Ok(())
        }
        Value::Float32(f) => {
            chain.push_bind(*f);
            Ok(())
        }
        Value::Float64(f) => {
            chain.push_bind(*f);
            Ok(())
        }
        Value::Text(s) => {
            chain.push_bind(s.clone());
            Ok(())
        }
        Value::Bytes(b) => {
            chain.push_bind(b.clone());
            Ok(())
        }
        Value::Date(d) => {
            chain.push_bind(*d);
            Ok(())
        }
        Value::Timestamp(ts) => {
            chain.push_bind(*ts);
            Ok(())
        }
        Value::Uuid(u) => {
            // QuestDB UUID columns accept Postgres UUID over pg-wire.
            chain.push_bind(*u);
            Ok(())
        }
        Value::Ipv4(a) => {
            // QuestDB IPv4 column accepts dotted-quad text over pg-wire.
            chain.push_bind(a.to_string());
            Ok(())
        }
        Value::Ipv6(_) => Err(BindError::UnsupportedType {
            column: column.to_string(),
            expected: format!("{dt}"),
            got_kind: "Ipv6 (QuestDB has no IPv6 column type)".to_string(),
        }),
        Value::Json(j) => {
            // QuestDB does not have a native JSON column — operators
            // route JSON into STRING. Serialise through the canonical
            // encoder so we never accidentally surface the cursor-envelope
            // shape produced by `serde_json::to_value(&Value)`.
            let s = serde_json::to_string(j).map_err(|source| BindError::Json {
                column: column.to_string(),
                source,
            })?;
            chain.push_bind(s);
            Ok(())
        }
        Value::BigInt(b) => {
            // No native BigInt in QuestDB — operators route via
            // `mapping.truncate=true` → `Float64` (handled by the runtime
            // matrix). Anything reaching here is a misconfiguration.
            Err(BindError::UnsupportedType {
                column: column.to_string(),
                expected: format!("{dt}"),
                got_kind: format!("BigInt(width={:?})", b.bits()),
            })
        }
        Value::Decimal(_) => Err(BindError::UnsupportedType {
            column: column.to_string(),
            expected: format!("{dt}"),
            got_kind: "Decimal".to_string(),
        }),
        Value::UInt8(_) | Value::UInt16(_) | Value::UInt32(_) | Value::UInt64(_) => {
            Err(BindError::UnsupportedType {
                column: column.to_string(),
                expected: format!("{dt}"),
                got_kind: "UInt*".to_string(),
            })
        }
        Value::Object(_) => {
            // Convert the structured document into JSON for binding.
            let j = air_elt_core::types::json_encode::value_to_json(value)
                .expect("Value::Object json encode must not fail after validation");
            chain.push_bind(j);
            Ok(())
        }
        Value::Custom(b) => bind_custom(chain, column, dt, b.as_ref()),
    }
}

fn bind_custom(
    chain: &mut Separated<'_, '_, Postgres, &str>,
    column: &str,
    dt: &DataType,
    value: &dyn air_elt_core::types::dynamic::DynValue,
) -> Result<(), BindError> {
    let any = value.as_any();
    if let Some(sym) = any.downcast_ref::<QuestDbSymbolValue>() {
        chain.push_bind(sym.0.clone());
        return Ok(());
    }
    if let Some(long256) = any.downcast_ref::<QuestDbLong256Value>() {
        chain.push_bind(long256.to_hex());
        return Ok(());
    }
    if let Some(geohash) = any.downcast_ref::<QuestDbGeohashValue>() {
        chain.push_bind(geohash.to_base32());
        return Ok(());
    }
    let kind = value.dyn_type();
    Err(BindError::UnsupportedType {
        column: column.to_string(),
        expected: format!("{dt}"),
        got_kind: kind.kind().to_string(),
    })
}

fn bind_null(
    chain: &mut Separated<'_, '_, Postgres, &str>,
    field: &Field,
) -> Result<(), BindError> {
    let dt = &field.data_type;
    match dt {
        DataType::Bool => {
            chain.push_bind::<Option<bool>>(None);
        }
        DataType::Int8 => {
            chain.push_bind::<Option<i16>>(None);
        }
        DataType::Int16 => {
            chain.push_bind::<Option<i16>>(None);
        }
        DataType::Int32 => {
            chain.push_bind::<Option<i32>>(None);
        }
        DataType::Int64 => {
            chain.push_bind::<Option<i64>>(None);
        }
        DataType::Float32 => {
            chain.push_bind::<Option<f32>>(None);
        }
        DataType::Float64 => {
            chain.push_bind::<Option<f64>>(None);
        }
        DataType::Text { .. } | DataType::Json => {
            chain.push_bind::<Option<String>>(None);
        }
        DataType::Bytes { .. } => {
            chain.push_bind::<Option<Vec<u8>>>(None);
        }
        DataType::Date => {
            chain.push_bind::<Option<NaiveDate>>(None);
        }
        DataType::Timestamp => {
            chain.push_bind::<Option<DateTime<Utc>>>(None);
        }
        DataType::Uuid => {
            chain.push_bind::<Option<Uuid>>(None);
        }
        DataType::Ipv4 => {
            chain.push_bind::<Option<String>>(None);
        }
        DataType::Custom(t) if is_questdb_native_kind(t.kind()) => {
            chain.push_bind::<Option<String>>(None);
        }
        DataType::BigInt { .. } | DataType::Decimal { .. } => {
            chain.push_bind::<Option<BigDecimal>>(None);
        }
        other => {
            return Err(BindError::UnsupportedType {
                column: field.name.clone(),
                expected: format!("{other}"),
                got_kind: "Null".to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::types::geohash::QuestDbGeohashType;
    use crate::types::long256::QuestDbLong256Type;
    use crate::types::symbol::QuestDbSymbolType;
    use chrono::TimeZone;
    use sqlx::QueryBuilder;

    fn field(name: &str, dt: DataType, nullable: bool) -> Field {
        Field {
            name: name.to_string(),
            data_type: dt,
            nullable,
        }
    }

    fn run_bind(field: &Field, value: &Value) -> Result<String, BindError> {
        let mut qb: QueryBuilder<'_, Postgres> = QueryBuilder::new("SELECT ");
        {
            let mut sep = qb.separated(", ");
            bind_value_separated_pg(&mut sep, field, value)?;
        }
        Ok(qb.into_sql())
    }

    #[test]
    fn binds_bool() {
        let f = field("b", DataType::Bool, false);
        run_bind(&f, &Value::Bool(true)).unwrap();
    }

    #[test]
    fn binds_int_family() {
        run_bind(&field("i", DataType::Int8, false), &Value::Int8(1)).unwrap();
        run_bind(&field("i", DataType::Int16, false), &Value::Int16(2)).unwrap();
        run_bind(&field("i", DataType::Int32, false), &Value::Int32(3)).unwrap();
        run_bind(&field("i", DataType::Int64, false), &Value::Int64(4)).unwrap();
    }

    #[test]
    fn binds_floats_text_bytes() {
        run_bind(&field("f", DataType::Float32, false), &Value::Float32(1.5)).unwrap();
        run_bind(&field("f", DataType::Float64, false), &Value::Float64(2.5)).unwrap();
        run_bind(
            &field("s", DataType::Text { size: None }, false),
            &Value::Text("hi".to_string()),
        )
        .unwrap();
        run_bind(
            &field("b", DataType::Bytes { size: None }, false),
            &Value::Bytes(vec![1, 2, 3]),
        )
        .unwrap();
    }

    #[test]
    fn binds_temporal_uuid_json() {
        let d = NaiveDate::from_ymd_opt(2025, 5, 14).unwrap();
        let ts = Utc.timestamp_opt(1, 0).unwrap();
        let u = Uuid::nil();
        run_bind(&field("d", DataType::Date, false), &Value::Date(d)).unwrap();
        run_bind(
            &field("t", DataType::Timestamp, false),
            &Value::Timestamp(ts),
        )
        .unwrap();
        run_bind(&field("u", DataType::Uuid, false), &Value::Uuid(u)).unwrap();
        run_bind(
            &field("j", DataType::Json, false),
            &Value::Json(serde_json::json!({"k": 1})),
        )
        .unwrap();
    }

    #[test]
    fn binds_custom_types_as_text() {
        // SYMBOL.
        run_bind(
            &field("s", DataType::Custom(Box::new(QuestDbSymbolType)), false),
            &Value::Custom(Box::new(QuestDbSymbolValue("apple".to_string()))),
        )
        .unwrap();
        // LONG256.
        run_bind(
            &field("l", DataType::Custom(Box::new(QuestDbLong256Type)), false),
            &Value::Custom(Box::new(QuestDbLong256Value([0u8; 32]))),
        )
        .unwrap();
        // IPv4 — now canonical, dispatched directly as Value::Ipv4.
        run_bind(
            &field("ip", DataType::Ipv4, false),
            &Value::Ipv4(std::net::Ipv4Addr::LOCALHOST),
        )
        .unwrap();
        // GEOHASH.
        run_bind(
            &field(
                "g",
                DataType::Custom(Box::new(QuestDbGeohashType { bits: 10 })),
                false,
            ),
            &Value::Custom(Box::new(QuestDbGeohashValue { bits: 10, value: 0 })),
        )
        .unwrap();
    }

    #[test]
    fn binds_null_for_each_known_type() {
        run_bind(&field("b", DataType::Bool, true), &Value::Null).unwrap();
        run_bind(&field("i", DataType::Int32, true), &Value::Null).unwrap();
        run_bind(
            &field("s", DataType::Text { size: None }, true),
            &Value::Null,
        )
        .unwrap();
        run_bind(
            &field("sym", DataType::Custom(Box::new(QuestDbSymbolType)), true),
            &Value::Null,
        )
        .unwrap();
    }

    #[test]
    fn rejects_unsigned() {
        let err = run_bind(&field("u", DataType::Int32, false), &Value::UInt32(1)).unwrap_err();
        assert!(matches!(err, BindError::UnsupportedType { .. }));
    }

    /// Non-zero LONG256: rendered as `0x` + 64 hex chars in big-endian
    /// byte order. The internal layout is little-endian so the highest
    /// byte must render first.
    #[test]
    fn long256_renders_nonzero_hex() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0xef; // LE byte 0 → low byte of value
        bytes[31] = 0xab; // LE byte 31 → high byte of value
        let v = QuestDbLong256Value(bytes);
        let hex = v.to_hex();
        assert!(hex.starts_with("0xab"), "expected 0xab… prefix, got {hex}");
        assert!(hex.ends_with("ef"), "expected …ef suffix, got {hex}");
        assert_eq!(hex.len(), 2 + 64);
    }

    /// IPv4 → dotted-quad bind path: confirm the canonical address
    /// round-trips through `bind_value_separated_pg` without error.
    #[test]
    fn ipv4_bind_round_trip_documentation_address() {
        let a = std::net::Ipv4Addr::new(203, 0, 113, 42);
        run_bind(&field("ip", DataType::Ipv4, false), &Value::Ipv4(a)).unwrap();
    }

    /// Non-zero geohash: a packed 35-bit value should re-emit "u4pruyd".
    #[test]
    fn geohash_renders_non_zero_base32() {
        const ALPHABET: &[u8; 32] = b"0123456789bcdefghjkmnpqrstuvwxyz";
        let mut packed: u64 = 0;
        for &c in b"u4pruyd" {
            let idx = ALPHABET
                .iter()
                .position(|&a| a == c)
                .expect("alphabet member") as u64;
            packed = (packed << 5) | idx;
        }
        let v = QuestDbGeohashValue {
            bits: 35,
            value: packed,
        };
        assert_eq!(v.to_base32(), "u4pruyd");
    }

    /// BigInt / Decimal / unsigned-int / unknown-custom values must all be
    /// rejected with `BindError::UnsupportedType` carrying a non-empty
    /// `got_kind`. The variant — not just the rejection — is load-bearing
    /// because the runner inspects it to decide between ctx-drop and a
    /// row-level type error.
    #[test]
    fn rejects_bigint_with_unsupported_type() {
        use num_bigint::BigInt as NumBigInt;
        let f = field("x", DataType::BigInt { width: Some(38) }, false);
        let v = Value::BigInt(NumBigInt::from(1_i64));
        let err = run_bind(&f, &v).unwrap_err();
        match err {
            BindError::UnsupportedType { got_kind, .. } => {
                assert!(!got_kind.is_empty(), "got_kind must describe the variant");
                assert!(got_kind.starts_with("BigInt"));
            }
            other => panic!("expected UnsupportedType, got {other:?}"),
        }
    }

    #[test]
    fn rejects_decimal_with_unsupported_type() {
        use bigdecimal::BigDecimal;
        use std::str::FromStr;
        let f = field(
            "x",
            DataType::Decimal {
                precision: Some(20),
                scale: Some(4),
            },
            false,
        );
        let v = Value::Decimal(BigDecimal::from_str("1.0").unwrap());
        let err = run_bind(&f, &v).unwrap_err();
        match err {
            BindError::UnsupportedType { got_kind, .. } => {
                assert_eq!(got_kind, "Decimal");
            }
            other => panic!("expected UnsupportedType, got {other:?}"),
        }
    }

    #[test]
    fn rejects_uint_variants_with_unsupported_type() {
        let f = field("x", DataType::Int32, false);
        for v in [
            Value::UInt8(1),
            Value::UInt16(1),
            Value::UInt32(1),
            Value::UInt64(1),
        ] {
            let err = run_bind(&f, &v).unwrap_err();
            match err {
                BindError::UnsupportedType { got_kind, .. } => {
                    assert_eq!(got_kind, "UInt*");
                }
                other => panic!("expected UnsupportedType for {v:?}, got {other:?}"),
            }
        }
    }

    /// An unrecognised custom value (kind that is not one of QuestDB's
    /// native customs) must be rejected with `UnsupportedType` carrying
    /// the rejected `kind` string.
    #[test]
    fn rejects_unknown_custom_with_unsupported_type() {
        use air_elt_core::types::convert::ConvertError;
        use air_elt_core::types::convert::context::ConversionContext;
        use air_elt_core::types::dynamic::{DynType, DynValue};
        use std::any::Any;

        // A throw-away custom kind not registered in QuestDB.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct UnknownT;
        impl UnknownT {
            const KIND: &'static str = "test.unknown_kind";
        }
        impl DynType for UnknownT {
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn kind(&self) -> &str {
                Self::KIND
            }
            fn can_convert_to(&self, _t: &DataType, _: bool) -> bool {
                false
            }
            fn can_construct_from(&self, _s: &DataType, _: bool) -> bool {
                false
            }
            fn convert(
                &self,
                _v: Value,
                _t: &DataType,
                _c: &ConversionContext,
            ) -> Result<Value, ConvertError> {
                Err(ConvertError::Unsupported {
                    src: DataType::Custom(Box::new(*self)),
                    dst: DataType::Int32,
                })
            }
            fn construct(
                &self,
                _v: Value,
                s: &DataType,
                _c: &ConversionContext,
            ) -> Result<Value, ConvertError> {
                Err(ConvertError::Unsupported {
                    src: s.clone(),
                    dst: DataType::Custom(Box::new(*self)),
                })
            }
            fn clone_box(&self) -> Box<dyn DynType> {
                Box::new(*self)
            }
        }
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        struct UnknownV;
        impl DynValue for UnknownV {
            fn dyn_type(&self) -> Box<dyn DynType> {
                Box::new(UnknownT)
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn into_any(self: Box<Self>) -> Box<dyn Any> {
                self
            }
            fn eq_dyn(&self, other: &dyn DynValue) -> bool {
                other.as_any().downcast_ref::<UnknownV>().is_some()
            }
            fn clone_box(&self) -> Box<dyn DynValue> {
                Box::new(*self)
            }
        }

        let f = field("x", DataType::Custom(Box::new(UnknownT)), false);
        let v = Value::Custom(Box::new(UnknownV));
        let err = run_bind(&f, &v).unwrap_err();
        match err {
            BindError::UnsupportedType { got_kind, .. } => {
                assert_eq!(got_kind, UnknownT::KIND);
            }
            other => panic!("expected UnsupportedType, got {other:?}"),
        }
    }
}
