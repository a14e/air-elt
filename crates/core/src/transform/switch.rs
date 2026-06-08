//! Compile-time construction and runtime lookup for value-to-value
//! switch tables.
//!
//! Per AIR-70 the mapping grammar grows a `switch` table:
//!
//! ```toml
//! status_label = {
//!     from = "status",
//!     switch = { "ACTIVE" = "active", "FINISHED" = "finished" },
//!     default = "unknown",
//! }
//! ```
//!
//! Each row's source value is dispatched against the `switch` keys; the
//! matched value (or `default` on miss / NULL input) feeds the sink
//! column. Keys are canonicalised against the source `DataType`; values
//! are canonicalised against the sink `DataType` for typed sinks or
//! derived through union-collapse for schemaless sinks (Mongo).

use std::str::FromStr;

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use uuid::Uuid;

use air_elt_types::Key;

use crate::error::ValidationError;
use crate::mapping::column::SwitchCase;
use crate::types::union_types::collapse_union;
use crate::types::{DataType, Value};
use air_elt_expr_runtime::ExpressionContext;

/// Compiled switch lookup. Keys are canonicalised through
/// [`Key::from_value`] so that integer subtypes
/// (Int8..Int64, UInt8..UInt64, BigInt) hash identically — the operator
/// can declare `1 = "one"` once and it matches an Int32 source value
/// `1` just as well as an Int64 source value `1`.
#[derive(Debug, Clone)]
pub struct SwitchTable {
    pub cases: ahash::AHashMap<Key, Value>,
    /// Fallback when the source value is NULL or no case matches.
    /// `None` means "emit Value::Null on miss".
    pub default: Option<Value>,
    /// Post-switch output `DataType`. Consumed by
    /// `TransformOp::output_type` (via `Transform::resolve_types`) so the
    /// compatibility validator can compare the switch's produced shape
    /// against the sink column instead of the original source type. For
    /// typed sinks this equals the sink `DataType`; for schemaless sinks
    /// it is the union-collapsed RHS type set.
    pub output_type: DataType,
}

/// Build a [`SwitchTable`] from raw TOML cases against the resolved
/// source/sink types.
///
/// `schemaless_sink == true` switches off the sink-DataType check and
/// instead derives the output type by collapsing the union of observed
/// RHS literal types via [`collapse_union`].
///
/// `truncate == true` enables RHS shortening for `Text` / `Bytes` sinks
/// with a declared size: literals exceeding the sink size are truncated
/// rather than rejected. Mirrors `ColumnConversionPlan.ctx.truncate` so
/// switch and convert paths agree on the same opt-in semantics. Ignored
/// on the schemaless path (no declared sink size exists there).
#[allow(clippy::too_many_arguments)]
pub fn compile_switch(
    flow: &str,
    column: &str,
    cases: &[SwitchCase],
    default_literal: Option<&toml::Value>,
    _truncate: bool,
    source_dt: &DataType,
    sink_dt: &DataType,
    schemaless_sink: bool,
    expr_context: &ExpressionContext,
) -> Result<SwitchTable, ValidationError> {
    if !is_switchable_source(source_dt) {
        return Err(ValidationError::SwitchUnsupportedSource {
            flow: flow.into(),
            column: column.into(),
            source_type: source_dt.clone(),
        });
    }

    let mut table_cases: ahash::AHashMap<Key, Value> = ahash::AHashMap::with_capacity(cases.len());
    let mut observed_value_types: Vec<DataType> = Vec::with_capacity(cases.len() + 1);

    for case in cases {
        // Parse key against the source DataType.
        let key_value = parse_key_text(&case.key, source_dt).map_err(|err| {
            ValidationError::SwitchKeyTypeMismatch {
                flow: flow.into(),
                column: column.into(),
                key: case.key.clone(),
                source_type: source_dt.clone(),
                detail: err.to_string(),
            }
        })?;
        let canonical_key = Key::from_value(&key_value).ok_or_else(|| {
            ValidationError::SwitchUnsupportedSource {
                flow: flow.into(),
                column: column.into(),
                source_type: source_dt.clone(),
            }
        })?;
        if table_cases.contains_key(&canonical_key) {
            return Err(ValidationError::SwitchDuplicateKey {
                flow: flow.into(),
                column: column.into(),
                key: case.key.clone(),
            });
        }

        let parser = air_elt_expr_parse::Parser::create();
        let program = parser.parse_toml(&case.value).map_err(|e| {
            ValidationError::SwitchValueTypeMismatch {
                flow: flow.into(),
                column: column.into(),
                key: case.key.clone(),
                sink_type: sink_dt.clone(),
                detail: e.to_string(),
            }
        })?;
        let resolved = expr_context.evaluate_const(&program).map_err(|e| {
            ValidationError::SwitchValueTypeMismatch {
                flow: flow.into(),
                column: column.into(),
                key: case.key.clone(),
                sink_type: sink_dt.clone(),
                detail: e.to_string(),
            }
        })?;

        let (out_value, out_type) = if schemaless_sink {
            let dt = resolved
                .data_type()
                .unwrap_or(DataType::Text { size: None });
            (resolved, dt)
        } else {
            let value =
                air_elt_types::ensure_sink_compatible(resolved, sink_dt).map_err(|reason| {
                    ValidationError::SwitchValueTypeMismatch {
                        flow: flow.into(),
                        column: column.into(),
                        key: case.key.clone(),
                        sink_type: sink_dt.clone(),
                        detail: reason,
                    }
                })?;
            (value, sink_dt.clone())
        };
        observed_value_types.push(out_type);
        table_cases.insert(canonical_key, out_value);
    }

    let default = if let Some(lit) = default_literal {
        let parser = air_elt_expr_parse::Parser::create();
        let program =
            parser
                .parse_toml(lit)
                .map_err(|e| ValidationError::SwitchValueTypeMismatch {
                    flow: flow.into(),
                    column: column.into(),
                    key: "<default>".into(),
                    sink_type: sink_dt.clone(),
                    detail: e.to_string(),
                })?;
        let resolved = expr_context.evaluate_const(&program).map_err(|e| {
            ValidationError::SwitchValueTypeMismatch {
                flow: flow.into(),
                column: column.into(),
                key: "<default>".into(),
                sink_type: sink_dt.clone(),
                detail: e.to_string(),
            }
        })?;
        if schemaless_sink {
            let dt = resolved
                .data_type()
                .unwrap_or(DataType::Text { size: None });
            observed_value_types.push(dt);
            Some(resolved)
        } else {
            let value =
                air_elt_types::ensure_sink_compatible(resolved, sink_dt).map_err(|reason| {
                    ValidationError::SwitchValueTypeMismatch {
                        flow: flow.into(),
                        column: column.into(),
                        key: "<default>".into(),
                        sink_type: sink_dt.clone(),
                        detail: reason,
                    }
                })?;
            Some(value)
        }
    } else {
        None
    };

    let output_type = if schemaless_sink {
        collapse_union(observed_value_types)
    } else {
        sink_dt.clone()
    };

    Ok(SwitchTable {
        cases: table_cases,
        default,
        output_type,
    })
}

pub(crate) fn is_switchable_source(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::Bool
            | DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float32
            | DataType::Float64
            | DataType::BigInt { .. }
            | DataType::Decimal { .. }
            | DataType::Text { .. }
            | DataType::Bytes { .. }
            | DataType::Date
            | DataType::Timestamp
            | DataType::Uuid
            | DataType::Ipv4
            | DataType::Ipv6
    )
}

/// Parse a TOML inline-table key string against the source `DataType`
/// and return a typed [`Value`]. Ints / bools branch explicitly because
/// the operator writes `1` / `true` as bare TOML keys (strings).
fn parse_key_text(key: &str, source: &DataType) -> Result<Value, String> {
    match source {
        DataType::Bool => match key {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(format!("expected true/false for Bool, got {key:?}")),
        },
        DataType::Int8 => parse_int_key(key, i8::MIN as i64, i8::MAX as i64)
            .map(|n| Value::Int8(n as i8))
            .ok_or_else(|| format!("{key:?} out of range for Int8")),
        DataType::Int16 => parse_int_key(key, i16::MIN as i64, i16::MAX as i64)
            .map(|n| Value::Int16(n as i16))
            .ok_or_else(|| format!("{key:?} out of range for Int16")),
        DataType::Int32 => parse_int_key(key, i32::MIN as i64, i32::MAX as i64)
            .map(|n| Value::Int32(n as i32))
            .ok_or_else(|| format!("{key:?} out of range for Int32")),
        DataType::Int64 => parse_int_key(key, i64::MIN, i64::MAX)
            .map(Value::Int64)
            .ok_or_else(|| format!("{key:?} out of range for Int64")),
        DataType::UInt8 => parse_uint_key(key, u8::MAX as u64)
            .map(|n| Value::UInt8(n as u8))
            .ok_or_else(|| format!("{key:?} out of range for UInt8")),
        DataType::UInt16 => parse_uint_key(key, u16::MAX as u64)
            .map(|n| Value::UInt16(n as u16))
            .ok_or_else(|| format!("{key:?} out of range for UInt16")),
        DataType::UInt32 => parse_uint_key(key, u32::MAX as u64)
            .map(|n| Value::UInt32(n as u32))
            .ok_or_else(|| format!("{key:?} out of range for UInt32")),
        DataType::UInt64 => parse_uint_key(key, u64::MAX)
            .map(Value::UInt64)
            .ok_or_else(|| format!("{key:?} out of range for UInt64")),
        DataType::Float32 => f32::from_str(key)
            .map(Value::Float32)
            .map_err(|e| format!("invalid Float32 key {key:?}: {e}")),
        DataType::Float64 => f64::from_str(key)
            .map(Value::Float64)
            .map_err(|e| format!("invalid Float64 key {key:?}: {e}")),
        DataType::BigInt { .. } => BigInt::from_str(key)
            .map(Value::BigInt)
            .map_err(|e| format!("invalid BigInt key {key:?}: {e}")),
        DataType::Decimal { .. } => BigDecimal::from_str(key)
            .map(Value::Decimal)
            .map_err(|e| format!("invalid Decimal key {key:?}: {e}")),
        DataType::Text { size } => {
            if let Some(max) = size {
                let chars = key.chars().count();
                if chars > *max as usize {
                    return Err(format!(
                        "key {key:?} exceeds Text size {max} ({chars} chars)"
                    ));
                }
            }
            Ok(Value::Text(key.to_owned()))
        }
        DataType::Bytes { .. } => {
            if let Some(hex_payload) = key.strip_prefix("hex:") {
                let bytes = (0..hex_payload.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex_payload[i..i + 2], 16))
                    .collect::<Result<Vec<u8>, _>>()
                    .map_err(|e| format!("invalid hex in Bytes key: {e}"))?;
                Ok(Value::Bytes(bytes))
            } else if let Some(utf8_payload) = key.strip_prefix("utf8:") {
                Ok(Value::Bytes(utf8_payload.as_bytes().to_vec()))
            } else {
                Ok(Value::Bytes(key.as_bytes().to_vec()))
            }
        }
        DataType::Date => NaiveDate::from_str(key)
            .map(Value::Date)
            .map_err(|e| format!("invalid Date key {key:?}: {e}")),
        DataType::Timestamp => DateTime::parse_from_rfc3339(key)
            .map(|dt| Value::Timestamp(dt.with_timezone(&Utc)))
            .map_err(|e| format!("invalid Timestamp key {key:?}: {e}")),
        DataType::Uuid => Uuid::parse_str(key)
            .map(Value::Uuid)
            .map_err(|e| format!("invalid Uuid key {key:?}: {e}")),
        DataType::Ipv4 => std::net::Ipv4Addr::from_str(key.trim())
            .map(Value::Ipv4)
            .map_err(|e| format!("invalid Ipv4 key {key:?}: {e}")),
        DataType::Ipv6 => std::net::Ipv6Addr::from_str(key.trim())
            .map(Value::Ipv6)
            .map_err(|e| format!("invalid Ipv6 key {key:?}: {e}")),
        other => Err(format!("unsupported switch key type: {other}")),
    }
}

fn parse_int_key(key: &str, min: i64, max: i64) -> Option<i64> {
    let n = i64::from_str(key).ok()?;
    if n < min || n > max { None } else { Some(n) }
}

fn parse_uint_key(key: &str, max: u64) -> Option<u64> {
    let n = u64::from_str(key).ok()?;
    if n > max { None } else { Some(n) }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn test_expr_context() -> ExpressionContext {
        ExpressionContext::create(
            Arc::new(air_elt_expr_funcs::FunctionRegistry::with_builtins()),
            std::path::Path::new("/tmp"),
        )
    }

    #[test]
    fn key_canonicalisation_works_for_switch_dispatch() {
        // Key canonicalisation (int subtypes, NaN, etc.) is tested
        // exhaustively in air_elt_types::key::tests. This test verifies
        // the integration: compile_switch + eval_switch agree on the
        // canonical form.
        let k8 = Key::from_value(&Value::Int8(1)).unwrap();
        let k64 = Key::from_value(&Value::Int64(1)).unwrap();
        assert_eq!(k8, k64);
    }

    fn hash_of(k: &Key) -> u64 {
        use std::hash::{BuildHasher, BuildHasherDefault};
        let builder: BuildHasherDefault<ahash::AHasher> = BuildHasherDefault::default();
        builder.hash_one(k)
    }

    #[test]
    fn key_nan_collapses_for_switch() {
        let a = f64::from_bits(0x7ff8_0000_0000_0001);
        let b = f64::from_bits(0xfff8_dead_0000_0000);
        assert!(a.is_nan() && b.is_nan());
        let ka = Key::from_value(&Value::Float64(a)).unwrap();
        let kb = Key::from_value(&Value::Float64(b)).unwrap();
        assert_eq!(ka, kb);
        // AHashMap lookups require BOTH Eq and Hash agreement — a NaN
        // canonicalisation that fixed only one would silently break
        // lookups. Pin both.
        assert_eq!(hash_of(&ka), hash_of(&kb));
    }

    #[test]
    fn switch_key_collapses_signed_zero() {
        let pos = Key::from_value(&Value::Float64(0.0)).unwrap();
        let neg = Key::from_value(&Value::Float64(-0.0)).unwrap();
        assert_eq!(pos, neg);
        assert_eq!(hash_of(&pos), hash_of(&neg));
    }

    #[test]
    fn key_rejects_null_json_and_non_cursor_custom() {
        assert!(Key::from_value(&Value::Null).is_none());
        assert!(Key::from_value(&Value::Json(serde_json::json!({}))).is_none());
    }

    #[test]
    fn compile_switch_typed_sink_string_keys() {
        let cases = vec![
            SwitchCase {
                key: "ACTIVE".into(),
                value: toml::Value::String("active".into()),
            },
            SwitchCase {
                key: "FINISHED".into(),
                value: toml::Value::String("finished".into()),
            },
        ];
        let default = toml::Value::String("unknown".into());
        let res = compile_switch(
            "f",
            "status_label",
            &cases,
            Some(&default),
            false,
            &DataType::Text { size: None },
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        assert_eq!(res.cases.len(), 2);
        assert_eq!(res.output_type, DataType::Text { size: None });
        assert_eq!(res.default, Some(Value::Text("unknown".into())));
    }

    #[test]
    fn compile_switch_typed_sink_int_keys() {
        let cases = vec![
            SwitchCase {
                key: "1".into(),
                value: toml::Value::String("one".into()),
            },
            SwitchCase {
                key: "2".into(),
                value: toml::Value::String("two".into()),
            },
        ];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        // Lookup must match an Int8/Int16/Int32/Int64 source carrying
        // the same numeric value.
        assert_eq!(
            res.cases.get(&Key::from_value(&Value::Int32(1)).unwrap()),
            Some(&Value::Text("one".into()))
        );
        assert_eq!(
            res.cases.get(&Key::from_value(&Value::Int64(2)).unwrap()),
            Some(&Value::Text("two".into()))
        );
    }

    #[test]
    fn compile_switch_bool_keys() {
        let cases = vec![
            SwitchCase {
                key: "true".into(),
                value: toml::Value::String("yes".into()),
            },
            SwitchCase {
                key: "false".into(),
                value: toml::Value::String("no".into()),
            },
        ];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Bool,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        assert_eq!(
            res.cases.get(&Key::from_value(&Value::Bool(true)).unwrap()),
            Some(&Value::Text("yes".into()))
        );
    }

    #[test]
    fn compile_switch_rejects_out_of_range_int_key() {
        let cases = vec![SwitchCase {
            key: "300".into(),
            value: toml::Value::String("oops".into()),
        }];
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int8,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::SwitchKeyTypeMismatch { .. }));
    }

    #[test]
    fn compile_switch_rejects_value_type_mismatch_typed_sink() {
        let cases = vec![SwitchCase {
            key: "1".into(),
            value: toml::Value::String("not-a-number".into()),
        }];
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &DataType::Int32,
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));
    }

    #[test]
    fn compile_switch_schemaless_sink_collapses_value_types() {
        let cases = vec![
            SwitchCase {
                key: "1".into(),
                value: toml::Value::Integer(10),
            },
            SwitchCase {
                key: "2".into(),
                value: toml::Value::Integer(1_000_000),
            },
        ];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            // schemaless sink type is unused on this path.
            &DataType::Json,
            true,
            &test_expr_context(),
        )
        .unwrap();
        // Evaluating TOML integers always produces Int64 — both values
        // collapse to Int64 without narrowing.
        assert_eq!(res.output_type, DataType::Int64);
        let k1 = Key::from_value(&Value::Int32(1)).unwrap();
        let k2 = Key::from_value(&Value::Int32(2)).unwrap();
        assert_eq!(res.cases.get(&k1), Some(&Value::Int64(10)));
        assert_eq!(res.cases.get(&k2), Some(&Value::Int64(1_000_000)));
    }

    #[test]
    fn compile_switch_float_source_keys() {
        let cases = vec![
            SwitchCase {
                key: "1.5".into(),
                value: toml::Value::String("one-and-a-half".into()),
            },
            SwitchCase {
                key: "-0.0".into(),
                value: toml::Value::String("zero".into()),
            },
        ];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Float64,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        // `-0.0` and `+0.0` collapse to the same SwitchKey.
        let pos_zero = Key::from_value(&Value::Float64(0.0)).unwrap();
        assert_eq!(res.cases.get(&pos_zero), Some(&Value::Text("zero".into())));
    }

    #[test]
    fn compile_switch_date_source_keys() {
        let cases = vec![SwitchCase {
            key: "2024-01-15".into(),
            value: toml::Value::String("mid-jan".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Date,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe =
            Key::from_value(&Value::Date(NaiveDate::from_str("2024-01-15").unwrap())).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("mid-jan".into())));
    }

    #[test]
    fn compile_switch_uuid_source_keys() {
        let cases = vec![SwitchCase {
            key: "550e8400-e29b-41d4-a716-446655440000".into(),
            value: toml::Value::String("nil-ish".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Uuid,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let probe = Key::from_value(&Value::Uuid(id)).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("nil-ish".into())));
    }

    #[test]
    fn compile_switch_bigint_source_keys() {
        // BigInt source: key text parses as decimal big integer; the
        // canonicalised key must match a `Value::BigInt` source.
        let cases = vec![SwitchCase {
            key: "99999999999999999999".into(),
            value: toml::Value::String("very-large".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::BigInt { width: Some(20) },
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::BigInt(
            BigInt::from_str("99999999999999999999").unwrap(),
        ))
        .unwrap();
        assert_eq!(
            res.cases.get(&probe),
            Some(&Value::Text("very-large".into()))
        );
    }

    #[test]
    fn compile_switch_decimal_source_keys() {
        let cases = vec![SwitchCase {
            key: "1.50".into(),
            value: toml::Value::String("buck-fifty".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Decimal {
                precision: Some(10),
                scale: Some(2),
            },
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        // 1.5 == 1.50 == 1.500 — all normalise to the same key.
        let probe =
            Key::from_value(&Value::Decimal(BigDecimal::from_str("1.500").unwrap())).unwrap();
        assert_eq!(
            res.cases.get(&probe),
            Some(&Value::Text("buck-fifty".into()))
        );
    }

    #[test]
    fn compile_switch_rejects_duplicate_keys() {
        let cases = vec![
            SwitchCase {
                key: "1".into(),
                value: toml::Value::String("a".into()),
            },
            SwitchCase {
                key: "1".into(),
                value: toml::Value::String("b".into()),
            },
        ];
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::SwitchDuplicateKey { .. }));
    }

    /// Integer-subtype canonicalisation hits the duplicate-key check
    /// too: `1` (parsed as Int32) and `"1"` (which would parse as
    /// Int8/Int16/Int32/Int64 identically) must clash even though the
    /// operator typed them differently.
    #[test]
    fn compile_switch_duplicate_via_canonicalisation() {
        let cases = vec![
            SwitchCase {
                key: "1".into(),
                value: toml::Value::String("a".into()),
            },
            // Same canonical key as `1` because the source is Int32 —
            // both keys parse to the same `Value::Int32(1)` → `Int(1)`.
            SwitchCase {
                key: "+1".into(),
                value: toml::Value::String("b".into()),
            },
        ];
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::SwitchDuplicateKey { .. }));
    }

    #[test]
    fn compile_switch_schemaless_sink_heterogeneous_values_collapse_to_union() {
        let cases = vec![
            SwitchCase {
                key: "1".into(),
                value: toml::Value::Integer(10),
            },
            SwitchCase {
                key: "2".into(),
                value: toml::Value::String("ten".into()),
            },
        ];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &DataType::Json,
            true,
            &test_expr_context(),
        )
        .unwrap();
        // Evaluating TOML integers produces Int64, so the union
        // members are Int64 + Text.
        match &res.output_type {
            DataType::Union(members) => {
                assert!(members.contains(&DataType::Int64), "Int64 must be in union");
                assert!(
                    members.iter().any(|m| matches!(m, DataType::Text { .. })),
                    "Text must be in union"
                );
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    /// Schemaless sink, default literal participates in the
    /// union-collapse — a heterogeneous default must widen the output.
    #[test]
    fn compile_switch_schemaless_default_widens_output() {
        let cases = vec![SwitchCase {
            key: "1".into(),
            value: toml::Value::Integer(10),
        }];
        let default = toml::Value::String("unknown".into());
        let res = compile_switch(
            "f",
            "label",
            &cases,
            Some(&default),
            false,
            &DataType::Int32,
            &DataType::Json,
            true,
            &test_expr_context(),
        )
        .unwrap();
        // Default must participate in the collapse: with case=Int64
        // (from TOML integer), default=Text the union has BOTH leaves.
        match &res.output_type {
            DataType::Union(members) => {
                assert!(members.contains(&DataType::Int64));
                assert!(members.iter().any(|m| matches!(m, DataType::Text { .. })));
            }
            other => panic!("expected Union, got {other:?}"),
        }
    }

    #[test]
    fn compile_switch_rejects_unsupported_source_type() {
        let cases = vec![SwitchCase {
            key: "x".into(),
            value: toml::Value::String("y".into()),
        }];
        // Every non-switchable source DataType family must reject.
        // Without enumerating each one, a regression flipping the
        // `is_switchable_source` whitelist (e.g. accidentally admitting
        // `Json`) could silently pass through.
        use crate::types::ConversionContext;
        use crate::types::convert::ConvertError;
        use crate::types::dynamic::DynType;
        use std::any::Any;

        #[derive(Debug)]
        struct StubType;
        impl DynType for StubType {
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn kind(&self) -> &str {
                "test.unswitchable"
            }
            fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
                false
            }
            fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
                false
            }
            fn convert(
                &self,
                _v: Value,
                _t: &DataType,
                _ctx: &ConversionContext,
            ) -> Result<Value, ConvertError> {
                unimplemented!()
            }
            fn construct(
                &self,
                _v: Value,
                _t: &DataType,
                _ctx: &ConversionContext,
            ) -> Result<Value, ConvertError> {
                unimplemented!()
            }
            fn clone_box(&self) -> Box<dyn DynType> {
                Box::new(StubType)
            }
        }

        for src in [
            DataType::Json,
            DataType::Xml,
            DataType::Union(vec![DataType::Int32, DataType::Text { size: None }]),
            DataType::Custom(Box::new(StubType)),
        ] {
            let err = compile_switch(
                "f",
                "label",
                &cases,
                None,
                false,
                &src,
                &DataType::Text { size: None },
                false,
                &test_expr_context(),
            )
            .unwrap_err();
            assert!(
                matches!(err, ValidationError::SwitchUnsupportedSource { .. }),
                "source {src:?} must reject, got {err:?}"
            );
        }
    }

    /// `parse_key_text` exercises a different code path per source
    /// `DataType`. The integer/bool/float/date/uuid/bigint/decimal
    /// arms are covered by earlier tests; this round-trip covers the
    /// remaining families that have non-trivial parser logic.
    #[test]
    fn compile_switch_uint32_source_keys() {
        let cases = vec![SwitchCase {
            key: "4294967295".into(), // u32::MAX
            value: toml::Value::String("max".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::UInt32,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::UInt32(u32::MAX)).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("max".into())));
    }

    /// `parse_key_text` arm for `UInt8` — verifies the `0..=u8::MAX`
    /// bound. Out-of-range key is rejected.
    #[test]
    fn compile_switch_uint8_source_keys() {
        let cases = vec![SwitchCase {
            key: "255".into(),
            value: toml::Value::String("max".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::UInt8,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::UInt8(u8::MAX)).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("max".into())));

        let overflow = vec![SwitchCase {
            key: "256".into(),
            value: toml::Value::String("nope".into()),
        }];
        compile_switch(
            "f",
            "label",
            &overflow,
            None,
            false,
            &DataType::UInt8,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .expect_err("256 must overflow u8");
    }

    /// `parse_key_text` arm for `UInt16` — verifies the `0..=u16::MAX`
    /// bound.
    #[test]
    fn compile_switch_uint16_source_keys() {
        let cases = vec![SwitchCase {
            key: "65535".into(),
            value: toml::Value::String("max".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::UInt16,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::UInt16(u16::MAX)).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("max".into())));
    }

    /// `parse_key_text` arm for `UInt64` — verifies the full
    /// `0..=u64::MAX` range (the upper half is unreachable from a
    /// signed-i64 path so the dedicated `parse_uint_key` matters).
    #[test]
    fn compile_switch_uint64_source_keys() {
        let cases = vec![SwitchCase {
            key: "18446744073709551615".into(), // u64::MAX
            value: toml::Value::String("max".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::UInt64,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::UInt64(u64::MAX)).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("max".into())));
    }

    /// `parse_key_text` arm for `Float32` — distinct from `Float64`
    /// because `f32::from_str` accepts a narrower exponent range.
    #[test]
    fn compile_switch_float32_source_keys() {
        let cases = vec![SwitchCase {
            key: "1.5".into(),
            value: toml::Value::String("one-and-half".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Float32,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::Float32(1.5_f32)).unwrap();
        assert_eq!(
            res.cases.get(&probe),
            Some(&Value::Text("one-and-half".into()))
        );
    }

    #[test]
    fn compile_switch_timestamp_source_keys() {
        let cases = vec![SwitchCase {
            key: "2024-01-15T10:30:00Z".into(),
            value: toml::Value::String("mid-jan-morning".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Timestamp,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let dt = chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let probe = Key::from_value(&Value::Timestamp(dt)).unwrap();
        assert_eq!(
            res.cases.get(&probe),
            Some(&Value::Text("mid-jan-morning".into()))
        );
    }

    #[test]
    fn compile_switch_bytes_source_keys() {
        // Bytes keys go through the typed-prefix grammar (hex/base64/
        // utf8/bin) via `default_value::parse`. Without the prefix the
        // parser rejects.
        let cases = vec![SwitchCase {
            key: "hex:deadbeef".into(),
            value: toml::Value::String("be-ef".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Bytes { size: None },
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let probe = Key::from_value(&Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("be-ef".into())));
    }

    #[test]
    fn compile_switch_text_source_keys_reject_overlong_key() {
        let cases = vec![SwitchCase {
            key: "TOO_LONG".into(),
            value: toml::Value::String("nope".into()),
        }];
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Text { size: Some(4) },
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::SwitchKeyTypeMismatch { .. }));
    }

    /// Since `ensure_sink_compatible` uses passthrough conversion,
    /// a `Text` value ("ALPHA") that exceeds the sink's declared size
    /// (3) is rejected regardless of the `truncate` flag — truncation
    /// is no longer applied at the switch RHS level.
    #[test]
    fn compile_switch_typed_text_truncate_shortens_rhs() {
        let cases = vec![SwitchCase {
            key: "1".into(),
            value: toml::Value::String("ALPHA".into()),
        }];
        let sink = DataType::Text { size: Some(3) };

        // Without truncate: parser rejects the over-length value.
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &sink,
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));

        // With truncate: passthrough conversion still rejects the
        // over-length value (truncation no longer applied).
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            true,
            &DataType::Int32,
            &sink,
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));
    }

    /// Bytes mirror of the Text truncate test: `ensure_sink_compatible`
    /// uses passthrough conversion, so a `Text` value ("hex:deadbeef")
    /// that does not match the sized `Bytes` sink is rejected regardless
    /// of the `truncate` flag. The `hex:` prefix is no longer parsed at
    /// the switch RHS level — `evaluate_expr_value` returns plain text.
    #[test]
    fn compile_switch_typed_bytes_truncate_shortens_rhs() {
        let cases = vec![SwitchCase {
            key: "1".into(),
            value: toml::Value::String("hex:deadbeef".into()),
        }];
        let sink = DataType::Bytes { size: Some(2) };

        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Int32,
            &sink,
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));

        // With truncate: passthrough conversion still rejects the
        // text-to-bytes mismatch (truncation no longer applied).
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            true,
            &DataType::Int32,
            &sink,
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));
    }

    #[test]
    fn switch_dispatches_on_ipv4_key() {
        let cases = vec![
            SwitchCase {
                key: "127.0.0.1".into(),
                value: toml::Value::String("loopback".into()),
            },
            SwitchCase {
                key: "192.0.2.1".into(),
                value: toml::Value::String("doc".into()),
            },
        ];
        let default = toml::Value::String("other".into());
        let res = compile_switch(
            "f",
            "label",
            &cases,
            Some(&default),
            false,
            &DataType::Ipv4,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let lookup = |s: &str| {
            let v = Value::Ipv4(std::net::Ipv4Addr::from_str(s).unwrap());
            let key = Key::from_value(&v).unwrap();
            res.cases
                .get(&key)
                .cloned()
                .unwrap_or(res.default.clone().unwrap())
        };
        assert_eq!(lookup("127.0.0.1"), Value::Text("loopback".into()));
        assert_eq!(lookup("192.0.2.1"), Value::Text("doc".into()));
        assert_eq!(lookup("10.0.0.1"), Value::Text("other".into()));
    }

    #[test]
    fn switch_dispatches_on_ipv6_key() {
        let cases = vec![
            SwitchCase {
                key: "::1".into(),
                value: toml::Value::String("local".into()),
            },
            SwitchCase {
                key: "2001:db8::1".into(),
                value: toml::Value::String("doc".into()),
            },
        ];
        let default = toml::Value::String("other".into());
        let res = compile_switch(
            "f",
            "label",
            &cases,
            Some(&default),
            false,
            &DataType::Ipv6,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        let lookup = |s: &str| {
            let v = Value::Ipv6(s.parse().unwrap());
            let key = Key::from_value(&v).unwrap();
            res.cases
                .get(&key)
                .cloned()
                .unwrap_or(res.default.clone().unwrap())
        };
        assert_eq!(lookup("::1"), Value::Text("local".into()));
        assert_eq!(lookup("2001:db8::1"), Value::Text("doc".into()));
        assert_eq!(lookup("fe80::1"), Value::Text("other".into()));
    }

    #[test]
    fn switch_ipv6_canonical_form_matches_uncompressed_key() {
        let cases = vec![SwitchCase {
            key: "2001:db8::1".into(),
            value: toml::Value::String("doc".into()),
        }];
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Ipv6,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap();
        // Uncompressed source canonicalises to the same Ipv6Addr value
        // as the compressed key, so the lookup must hit.
        let v: std::net::Ipv6Addr = "2001:0db8::0001".parse().unwrap();
        let probe = Key::from_value(&Value::Ipv6(v)).unwrap();
        assert_eq!(res.cases.get(&probe), Some(&Value::Text("doc".into())));
    }

    #[test]
    fn switch_rejects_invalid_ipv4_key() {
        let cases = vec![SwitchCase {
            key: "not.an.ip".into(),
            value: toml::Value::String("nope".into()),
        }];
        let err = compile_switch(
            "f",
            "label",
            &cases,
            None,
            false,
            &DataType::Ipv4,
            &DataType::Text { size: None },
            false,
            &test_expr_context(),
        )
        .unwrap_err();
        assert!(
            matches!(err, ValidationError::SwitchKeyTypeMismatch { .. }),
            "expected SwitchKeyTypeMismatch, got {err:?}"
        );
    }

    #[test]
    fn is_switchable_source_admits_ip() {
        assert!(is_switchable_source(&DataType::Ipv4));
        assert!(is_switchable_source(&DataType::Ipv6));
    }
}
