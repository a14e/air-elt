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

use std::hash::{Hash, Hasher};
use std::str::FromStr;

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use num_bigint::BigInt;
use num_traits::ToPrimitive;
use uuid::Uuid;

use crate::error::ValidationError;
use crate::mapping::column::SwitchCase;
use crate::types::default_value::{self, DefaultParseError};
use crate::types::union_types::collapse_union;
use crate::types::{DataType, Value};

/// Compiled switch lookup. Keys are canonicalised through
/// [`SwitchKey::from_value`] so that integer subtypes
/// (Int8..Int64, UInt8..UInt64, BigInt) hash identically — the operator
/// can declare `1 = "one"` once and it matches an Int32 source value
/// `1` just as well as an Int64 source value `1`.
#[derive(Debug, Clone)]
pub struct SwitchTable {
    pub cases: ahash::AHashMap<SwitchKey, Value>,
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

/// Closed set of canonical key shapes a switch table can dispatch on.
/// Every supported source variant collapses into exactly one of these
/// — distinct subtypes that compare equal at the value level (Int8(1)
/// vs Int64(1)) deliberately collapse to the same key.
///
/// Integer keys split across two arms for performance: the common case
/// fits in `i64` and uses [`SwitchKey::Int`] (no heap allocation); only
/// values that exceed `i64::MAX` (in practice from `UInt64` or a
/// genuinely-wide `Value::BigInt`) fall back to [`SwitchKey::BigInt`].
/// `Hash` and `Eq` are implemented manually so the two integer arms
/// canonicalise to the same hash and compare equal for any value that
/// fits in `i64` — operator-declared key `1` (which lands as `Int(1)`)
/// must match a source-side `Value::BigInt(1)` regardless of which
/// arm the constructor routes it through.
#[derive(Debug, Clone)]
pub enum SwitchKey {
    Bool(bool),
    /// Hot path — all integer subtypes that fit in `i64` (Int8..Int64,
    /// UInt8..UInt32, and `UInt64`/`BigInt` values within range).
    /// No heap allocation.
    Int(i64),
    /// Fallback — integer values outside the `i64` range. Hashed and
    /// compared against [`SwitchKey::Int`] in canonical (numerically
    /// equal) form, so the choice of arm is transparent to lookups.
    BigInt(num_bigint::BigInt),
    /// Bits-stable float key (`f64::to_bits`). Float32 widens to f64
    /// before hashing so `Float32(1.0)` and `Float64(1.0)` match.
    Float(u64),
    Text(String),
    Bytes(Vec<u8>),
    Date(chrono::NaiveDate),
    Timestamp(chrono::DateTime<chrono::Utc>),
    Uuid(uuid::Uuid),
    /// `BigDecimal`'s default Hash impl is missing, so we project to
    /// the normalised decimal-string form — same shape the cursor
    /// JSON storage uses.
    Decimal(String),
}

impl SwitchKey {
    /// Canonicalise a [`BigInt`] into the most compact key arm. Values
    /// that fit in `i64` collapse to [`SwitchKey::Int`] so they share a
    /// hash bucket with the hot-path integer constructors.
    fn from_bigint(b: num_bigint::BigInt) -> Self {
        if let Some(n) = b.to_i64() {
            SwitchKey::Int(n)
        } else {
            SwitchKey::BigInt(b)
        }
    }

    /// Project a runtime [`Value`] into a hashable key. Returns
    /// `None` for variants that cannot participate in switch dispatch
    /// (Null, Json, Custom). Validation guarantees that any value
    /// reaching `Transform::Switch` is one of the supported shapes,
    /// so `None` here surfaces as a `DerivedPlanInvariant`.
    pub fn from_value(v: &Value) -> Option<Self> {
        use Value::*;
        Some(match v {
            Null => return None,
            Bool(b) => SwitchKey::Bool(*b),
            // Signed and small-unsigned variants all fit in `i64`
            // unconditionally — skip the BigInt round-trip.
            Int8(n) => SwitchKey::Int(i64::from(*n)),
            Int16(n) => SwitchKey::Int(i64::from(*n)),
            Int32(n) => SwitchKey::Int(i64::from(*n)),
            Int64(n) => SwitchKey::Int(*n),
            UInt8(n) => SwitchKey::Int(i64::from(*n)),
            UInt16(n) => SwitchKey::Int(i64::from(*n)),
            UInt32(n) => SwitchKey::Int(i64::from(*n)),
            // `u64::MAX > i64::MAX`, so values above i64::MAX must
            // spill into the BigInt arm.
            UInt64(n) => match i64::try_from(*n) {
                Ok(i) => SwitchKey::Int(i),
                Err(_) => SwitchKey::BigInt(num_bigint::BigInt::from(*n)),
            },
            BigInt(b) => SwitchKey::from_bigint(b.clone()),
            Float32(f) => SwitchKey::Float(normalise_float_bits(f64::from(*f))),
            Float64(f) => SwitchKey::Float(normalise_float_bits(*f)),
            Text(s) => SwitchKey::Text(s.clone()),
            Bytes(b) => SwitchKey::Bytes(b.clone()),
            Date(d) => SwitchKey::Date(*d),
            Timestamp(t) => SwitchKey::Timestamp(*t),
            Uuid(u) => SwitchKey::Uuid(*u),
            Decimal(d) => SwitchKey::Decimal(d.normalized().to_string()),
            Json(_) | Custom(_) => return None,
        })
    }
}

impl PartialEq for SwitchKey {
    fn eq(&self, other: &Self) -> bool {
        use SwitchKey::*;
        // Fast path: same arm, same payload.
        match (self, other) {
            (Int(a), Int(b)) => return a == b,
            (BigInt(a), BigInt(b)) => return a == b,
            // Cross-arm integer comparison: a `BigInt` payload that
            // fits in `i64` must compare equal to the matching `Int`.
            (Int(a), BigInt(b)) | (BigInt(b), Int(a)) => {
                return b.to_i64().is_some_and(|n| n == *a);
            }
            _ => {}
        }
        match (self, other) {
            (Bool(a), Bool(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Text(a), Text(b)) => a == b,
            (Bytes(a), Bytes(b)) => a == b,
            (Date(a), Date(b)) => a == b,
            (Timestamp(a), Timestamp(b)) => a == b,
            (Uuid(a), Uuid(b)) => a == b,
            (Decimal(a), Decimal(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for SwitchKey {}

impl Hash for SwitchKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        use SwitchKey::*;
        // Per-arm discriminant prefixes keep different variants from
        // colliding in the hash. Both integer arms share prefix `0`
        // and canonicalise to `i64` whenever the value fits — so
        // `Int(1)` and `BigInt(BigInt::from(1))` hash identically
        // WITHOUT allocating a fresh BigInt per hash call. Only
        // genuinely-wide BigInts (those that fail `to_i64()`) reach
        // the BigInt::hash branch, where the heap allocation is
        // already paid for by the value's own representation.
        match self {
            Int(n) => {
                0u8.hash(state);
                n.hash(state);
            }
            BigInt(b) => {
                0u8.hash(state);
                match b.to_i64() {
                    Some(n) => n.hash(state),
                    None => b.hash(state),
                }
            }
            Bool(b) => {
                1u8.hash(state);
                b.hash(state);
            }
            Float(bits) => {
                2u8.hash(state);
                bits.hash(state);
            }
            Text(s) => {
                3u8.hash(state);
                s.hash(state);
            }
            Bytes(b) => {
                4u8.hash(state);
                b.hash(state);
            }
            Date(d) => {
                5u8.hash(state);
                d.hash(state);
            }
            Timestamp(t) => {
                6u8.hash(state);
                t.hash(state);
            }
            Uuid(u) => {
                7u8.hash(state);
                u.hash(state);
            }
            Decimal(s) => {
                8u8.hash(state);
                s.hash(state);
            }
        }
    }
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
    truncate: bool,
    source_dt: &DataType,
    sink_dt: &DataType,
    schemaless_sink: bool,
) -> Result<SwitchTable, ValidationError> {
    if !is_switchable_source(source_dt) {
        return Err(ValidationError::SwitchUnsupportedSource {
            flow: flow.into(),
            column: column.into(),
            source_type: source_dt.clone(),
        });
    }

    let mut table_cases: ahash::AHashMap<SwitchKey, Value> =
        ahash::AHashMap::with_capacity(cases.len());
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
        let canonical_key = SwitchKey::from_value(&key_value).ok_or_else(|| {
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

        // Parse value against the sink DataType (typed sink) or
        // untyped (schemaless).
        let (out_value, out_type) = if schemaless_sink {
            parse_value_untyped(&case.value)
        } else {
            let v = parse_typed_with_truncate(&case.value, sink_dt, truncate).map_err(|err| {
                ValidationError::SwitchValueTypeMismatch {
                    flow: flow.into(),
                    column: column.into(),
                    key: case.key.clone(),
                    sink_type: sink_dt.clone(),
                    detail: err.to_string(),
                }
            })?;
            (v, sink_dt.clone())
        };
        observed_value_types.push(out_type);
        table_cases.insert(canonical_key, out_value);
    }

    // Default. Typed sink: same parser as `default = ...` today.
    // Schemaless: untyped → folds into the union collapse.
    let default = if let Some(lit) = default_literal {
        if schemaless_sink {
            let (v, t) = parse_value_untyped(lit);
            observed_value_types.push(t);
            Some(v)
        } else {
            let v = parse_typed_with_truncate(lit, sink_dt, truncate).map_err(|err| {
                ValidationError::SwitchValueTypeMismatch {
                    flow: flow.into(),
                    column: column.into(),
                    key: "<default>".into(),
                    sink_type: sink_dt.clone(),
                    detail: err.to_string(),
                }
            })?;
            Some(v)
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

/// Parse a typed RHS literal against `sink_dt`, applying truncation for
/// `Text { size: Some(_) }` / `Bytes { size: Some(_) }` when
/// `truncate == true`. Without truncate this is exactly
/// `default_value::parse`; with truncate we pre-shorten the literal to
/// the declared size so the underlying parser's length check passes.
fn parse_typed_with_truncate(
    literal: &toml::Value,
    sink_dt: &DataType,
    truncate: bool,
) -> Result<Value, DefaultParseError> {
    if !truncate {
        return default_value::parse(literal, sink_dt);
    }
    match sink_dt {
        DataType::Text { size: Some(max) } => {
            // Only String literals participate in truncation; non-string
            // shapes (e.g. an integer fed to a Text column) keep the
            // standard TypeMismatch behaviour.
            if let Some(s) = literal.as_str() {
                let max_usize = *max as usize;
                let truncated: String = s.chars().take(max_usize).collect();
                return default_value::parse(&toml::Value::String(truncated), sink_dt);
            }
            default_value::parse(literal, sink_dt)
        }
        DataType::Bytes { size: Some(max) } => {
            // The Bytes default parser reads `hex:` / `base64:` / `utf8:`
            // / `bin:` prefixes off a String literal; we let it decode
            // first (without size truncation) and then shorten the
            // resulting byte vector to fit. Bypass the size guard by
            // re-parsing against a size-stripped clone of the type.
            let unbounded = DataType::Bytes { size: None };
            let parsed = default_value::parse(literal, &unbounded)?;
            let Value::Bytes(mut bytes) = parsed else {
                // `default_value::parse(..., Bytes { size: None })`
                // always yields `Value::Bytes`; this arm exists for
                // defensive typing only.
                return default_value::parse(literal, sink_dt);
            };
            let max_usize = *max as usize;
            if bytes.len() > max_usize {
                bytes.truncate(max_usize);
            }
            Ok(Value::Bytes(bytes))
        }
        _ => default_value::parse(literal, sink_dt),
    }
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
    )
}

/// Parse a TOML inline-table key string against the source `DataType`
/// and return a typed [`Value`]. Reuses [`default_value::parse`] for
/// the type families that accept string-quoted literals; ints / bools
/// branch explicitly because the operator expects to write `1` /
/// `true` as bare TOML keys (which always reach us as strings).
fn parse_key_text(key: &str, source: &DataType) -> Result<Value, DefaultParseError> {
    match source {
        DataType::Bool => match key {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => Err(DefaultParseError::TypeMismatch {
                dst: DataType::Bool,
            }),
        },
        DataType::Int8 => parse_int_key(key, i8::MIN as i64, i8::MAX as i64)
            .map(|n| Value::Int8(n as i8))
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::Int8,
            }),
        DataType::Int16 => parse_int_key(key, i16::MIN as i64, i16::MAX as i64)
            .map(|n| Value::Int16(n as i16))
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::Int16,
            }),
        DataType::Int32 => parse_int_key(key, i32::MIN as i64, i32::MAX as i64)
            .map(|n| Value::Int32(n as i32))
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::Int32,
            }),
        DataType::Int64 => parse_int_key(key, i64::MIN, i64::MAX)
            .map(Value::Int64)
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::Int64,
            }),
        DataType::UInt8 => parse_uint_key(key, u8::MAX as u64)
            .map(|n| Value::UInt8(n as u8))
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::UInt8,
            }),
        DataType::UInt16 => parse_uint_key(key, u16::MAX as u64)
            .map(|n| Value::UInt16(n as u16))
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::UInt16,
            }),
        DataType::UInt32 => parse_uint_key(key, u32::MAX as u64)
            .map(|n| Value::UInt32(n as u32))
            .ok_or(DefaultParseError::OutOfRange {
                dst: DataType::UInt32,
            }),
        DataType::UInt64 => {
            parse_uint_key(key, u64::MAX)
                .map(Value::UInt64)
                .ok_or(DefaultParseError::OutOfRange {
                    dst: DataType::UInt64,
                })
        }
        DataType::Float32 => {
            f32::from_str(key)
                .map(Value::Float32)
                .map_err(|_| DefaultParseError::TypeMismatch {
                    dst: DataType::Float32,
                })
        }
        DataType::Float64 => {
            f64::from_str(key)
                .map(Value::Float64)
                .map_err(|_| DefaultParseError::TypeMismatch {
                    dst: DataType::Float64,
                })
        }
        DataType::BigInt { width } => {
            BigInt::from_str(key)
                .map(Value::BigInt)
                .map_err(|_| DefaultParseError::OutOfRange {
                    dst: DataType::BigInt { width: *width },
                })
        }
        DataType::Decimal { precision, scale } => BigDecimal::from_str(key)
            .map(Value::Decimal)
            .map_err(|_| DefaultParseError::TypeMismatch {
                dst: DataType::Decimal {
                    precision: *precision,
                    scale: *scale,
                },
            }),
        DataType::Text { size } => {
            if let Some(max) = size {
                let chars = key.chars().count();
                if chars > *max as usize {
                    return Err(DefaultParseError::LengthExceeds {
                        got: chars,
                        max: *max as usize,
                    });
                }
            }
            Ok(Value::Text(key.to_string()))
        }
        DataType::Bytes { size: _ } => {
            // Reuse the Bytes prefix grammar by wrapping the key in a
            // toml::Value::String — `default_value::parse` already
            // handles hex:/base64:/utf8:/bin: prefixes against
            // `DataType::Bytes`.
            default_value::parse(&toml::Value::String(key.to_string()), source)
        }
        DataType::Date => {
            NaiveDate::from_str(key)
                .map(Value::Date)
                .map_err(|e| DefaultParseError::InvalidDate {
                    reason: e.to_string(),
                })
        }
        DataType::Timestamp => DateTime::parse_from_rfc3339(key)
            .map(|dt| Value::Timestamp(dt.with_timezone(&Utc)))
            .map_err(|e| DefaultParseError::InvalidTimestamp {
                reason: e.to_string(),
            }),
        DataType::Uuid => {
            Uuid::parse_str(key)
                .map(Value::Uuid)
                .map_err(|e| DefaultParseError::InvalidUuid {
                    reason: e.to_string(),
                })
        }
        DataType::Json | DataType::Xml | DataType::Union(_) | DataType::Custom(_) => {
            Err(DefaultParseError::TypeMismatch {
                dst: source.clone(),
            })
        }
    }
}

fn parse_int_key(key: &str, min: i64, max: i64) -> Option<i64> {
    let n = i64::from_str(key).ok()?;
    if n < min || n > max { None } else { Some(n) }
}

/// Canonical bit pattern for a float key. All NaN bit patterns collapse
/// onto a single sentinel so two `f64::NAN` source values hash equal —
/// `f64::to_bits` otherwise preserves the variant payload and the
/// signalling bit, leading to `NaN != NaN` lookups.
fn normalise_float_bits(f: f64) -> u64 {
    if f.is_nan() {
        f64::NAN.to_bits()
    } else if f == 0.0 {
        // Collapse `-0.0` onto `+0.0` so `Bool/Int → Float` widening
        // and operator-written `0.0` agree.
        0u64
    } else {
        f.to_bits()
    }
}

fn parse_uint_key(key: &str, max: u64) -> Option<u64> {
    let n = u64::from_str(key).ok()?;
    if n > max { None } else { Some(n) }
}

/// Parse a TOML literal RHS value without a target DataType — used for
/// schemaless sinks where the sink column type is derived from the
/// observed value set via [`collapse_union`].
fn parse_value_untyped(literal: &toml::Value) -> (Value, DataType) {
    use toml::Value::*;
    match literal {
        String(s) => (
            Value::Text(s.clone()),
            DataType::Text {
                size: Some(s.chars().count() as u32),
            },
        ),
        Integer(n) => {
            let v = *n;
            if (i8::MIN as i64..=i8::MAX as i64).contains(&v) {
                (Value::Int8(v as i8), DataType::Int8)
            } else if (i16::MIN as i64..=i16::MAX as i64).contains(&v) {
                (Value::Int16(v as i16), DataType::Int16)
            } else if (i32::MIN as i64..=i32::MAX as i64).contains(&v) {
                (Value::Int32(v as i32), DataType::Int32)
            } else {
                (Value::Int64(v), DataType::Int64)
            }
        }
        Float(f) => (Value::Float64(*f), DataType::Float64),
        Boolean(b) => (Value::Bool(*b), DataType::Bool),
        Datetime(d) => {
            let s = d.to_string();
            if let Ok(dt) = DateTime::parse_from_rfc3339(&s) {
                (
                    Value::Timestamp(dt.with_timezone(&Utc)),
                    DataType::Timestamp,
                )
            } else if let Ok(date) = NaiveDate::from_str(&s) {
                (Value::Date(date), DataType::Date)
            } else {
                (
                    Value::Text(s.clone()),
                    DataType::Text {
                        size: Some(s.len() as u32),
                    },
                )
            }
        }
        Array(_) | Table(_) => {
            let j = toml_to_json(literal);
            (Value::Json(j), DataType::Json)
        }
    }
}

fn toml_to_json(v: &toml::Value) -> serde_json::Value {
    use toml::Value::*;
    match v {
        String(s) => serde_json::Value::String(s.clone()),
        Integer(n) => serde_json::Value::Number((*n).into()),
        Float(f) => serde_json::Number::from_f64(*f)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Boolean(b) => serde_json::Value::Bool(*b),
        Datetime(d) => serde_json::Value::String(d.to_string()),
        Array(arr) => serde_json::Value::Array(arr.iter().map(toml_to_json).collect()),
        Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.clone(), toml_to_json(v)))
                .collect(),
        ),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn switch_key_collapses_integer_subtypes() {
        let one = SwitchKey::Int(1);
        assert_eq!(SwitchKey::from_value(&Value::Int8(1)).unwrap(), one);
        assert_eq!(SwitchKey::from_value(&Value::Int16(1)).unwrap(), one);
        assert_eq!(SwitchKey::from_value(&Value::Int32(1)).unwrap(), one);
        assert_eq!(SwitchKey::from_value(&Value::Int64(1)).unwrap(), one);
        assert_eq!(SwitchKey::from_value(&Value::UInt32(1)).unwrap(), one);
        // BigInt with a small payload collapses into the i64 hot-path arm.
        assert_eq!(
            SwitchKey::from_value(&Value::BigInt(BigInt::from(1))).unwrap(),
            one
        );
    }

    #[test]
    fn switch_key_collapses_int_and_bigint_arms_for_small_values() {
        // The constructor canonicalises `BigInt(1)` into `Int(1)`,
        // but operator-built keys may still land on the BigInt arm
        // directly. They must compare equal and hash identically.
        let int_arm = SwitchKey::Int(1);
        let bigint_arm = SwitchKey::BigInt(BigInt::from(1));
        assert_eq!(int_arm, bigint_arm);

        fn hash_of(k: &SwitchKey) -> u64 {
            use std::hash::{BuildHasher, BuildHasherDefault};
            let builder: BuildHasherDefault<ahash::AHasher> = BuildHasherDefault::default();
            builder.hash_one(k)
        }
        assert_eq!(hash_of(&int_arm), hash_of(&bigint_arm));
    }

    #[test]
    fn switch_key_uint64_overflow_lands_on_bigint_arm() {
        // `UInt64` values above `i64::MAX` must spill to BigInt.
        let big = u64::MAX;
        let key = SwitchKey::from_value(&Value::UInt64(big)).unwrap();
        // Payload must be `BigInt::from(u64::MAX)` exactly — a regression
        // where the conversion truncates to `i64` would still land on
        // the BigInt arm but with a wrong (negative) payload.
        match &key {
            SwitchKey::BigInt(b) => assert_eq!(*b, BigInt::from(big)),
            other => panic!("expected SwitchKey::BigInt, got {other:?}"),
        }
        // The matching operator-side key parsed from `"18446744073709551615"`
        // would also land on the BigInt arm via `from_bigint`.
        let parsed = SwitchKey::from_bigint(BigInt::from(big));
        assert_eq!(key, parsed);
    }

    fn hash_of(k: &SwitchKey) -> u64 {
        use std::hash::{BuildHasher, BuildHasherDefault};
        let builder: BuildHasherDefault<ahash::AHasher> = BuildHasherDefault::default();
        builder.hash_one(k)
    }

    #[test]
    fn switch_key_collapses_nan_bit_patterns() {
        let a = f64::from_bits(0x7ff8_0000_0000_0001);
        let b = f64::from_bits(0xfff8_dead_0000_0000);
        assert!(a.is_nan() && b.is_nan());
        let ka = SwitchKey::from_value(&Value::Float64(a)).unwrap();
        let kb = SwitchKey::from_value(&Value::Float64(b)).unwrap();
        assert_eq!(ka, kb);
        // AHashMap lookups require BOTH Eq and Hash agreement — a NaN
        // canonicalisation that fixed only one would silently break
        // lookups. Pin both.
        assert_eq!(hash_of(&ka), hash_of(&kb));
    }

    #[test]
    fn switch_key_collapses_signed_zero() {
        let pos = SwitchKey::from_value(&Value::Float64(0.0)).unwrap();
        let neg = SwitchKey::from_value(&Value::Float64(-0.0)).unwrap();
        assert_eq!(pos, neg);
        assert_eq!(hash_of(&pos), hash_of(&neg));
    }

    #[test]
    fn switch_key_rejects_null_and_custom_kinds() {
        use std::any::Any;

        // Minimal `DynValue` stand-in. Switch only inspects the
        // discriminant via `from_value`, so the trait bodies can be
        // unimplemented for everything but the type-system requirements.
        #[derive(Debug)]
        struct StubValue;
        impl crate::types::DynValue for StubValue {
            fn dyn_type(&self) -> Box<dyn crate::types::dynamic::DynType> {
                unimplemented!()
            }
            fn as_any(&self) -> &dyn Any {
                self
            }
            fn into_any(self: Box<Self>) -> Box<dyn Any> {
                self
            }
            fn eq_dyn(&self, _other: &dyn crate::types::DynValue) -> bool {
                true
            }
            fn clone_box(&self) -> Box<dyn crate::types::DynValue> {
                Box::new(StubValue)
            }
        }

        assert!(SwitchKey::from_value(&Value::Null).is_none());
        assert!(SwitchKey::from_value(&Value::Json(serde_json::json!({}))).is_none());
        assert!(SwitchKey::from_value(&Value::Custom(Box::new(StubValue))).is_none());
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
        )
        .unwrap();
        // Lookup must match an Int8/Int16/Int32/Int64 source carrying
        // the same numeric value.
        assert_eq!(
            res.cases
                .get(&SwitchKey::from_value(&Value::Int32(1)).unwrap()),
            Some(&Value::Text("one".into()))
        );
        assert_eq!(
            res.cases
                .get(&SwitchKey::from_value(&Value::Int64(2)).unwrap()),
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
        )
        .unwrap();
        assert_eq!(
            res.cases
                .get(&SwitchKey::from_value(&Value::Bool(true)).unwrap()),
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
        )
        .unwrap();
        // Int8 + Int32 should collapse to Int32 by widening.
        assert_eq!(res.output_type, DataType::Int32);
        // Values must round-trip with the widened-then-narrowed types
        // that `parse_value_untyped` produced: 10 fits in Int8, the
        // larger value lands in Int32. A regression where the value
        // arm silently widens both to Int32 (or narrows both to Int8
        // with overflow) would still pass `output_type == Int32` —
        // assert the per-key Value directly.
        let k1 = SwitchKey::from_value(&Value::Int32(1)).unwrap();
        let k2 = SwitchKey::from_value(&Value::Int32(2)).unwrap();
        assert_eq!(res.cases.get(&k1), Some(&Value::Int8(10)));
        assert_eq!(res.cases.get(&k2), Some(&Value::Int32(1_000_000)));
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
        )
        .unwrap();
        // `-0.0` and `+0.0` collapse to the same SwitchKey.
        let pos_zero = SwitchKey::from_value(&Value::Float64(0.0)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::Date(NaiveDate::from_str("2024-01-15").unwrap()))
            .unwrap();
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
        )
        .unwrap();
        let id = Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap();
        let probe = SwitchKey::from_value(&Value::Uuid(id)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::BigInt(
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
        )
        .unwrap();
        // 1.5 == 1.50 == 1.500 — all normalise to the same key.
        let probe =
            SwitchKey::from_value(&Value::Decimal(BigDecimal::from_str("1.500").unwrap())).unwrap();
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
        )
        .unwrap();
        // Union shape alone is not enough — assert both leaves are
        // present. A regression that emitted `Union(vec![Int8])` (a
        // single-leaf degenerate union) would still match the broad
        // `Union(_)` pattern.
        match &res.output_type {
            DataType::Union(members) => {
                assert!(members.contains(&DataType::Int8), "Int8 must be in union");
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
        )
        .unwrap();
        // Default must participate in the collapse: with case=Int8,
        // default=Text the union has BOTH leaves. A regression that
        // dropped the default from `observed_value_types` would give
        // a plain Int8 (no union), which the broad `Union(_)` pattern
        // would have caught — but we tighten the assertion anyway to
        // catch the inverse drift (default-only emission).
        match &res.output_type {
            DataType::Union(members) => {
                assert!(members.contains(&DataType::Int8));
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::UInt32(u32::MAX)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::UInt8(u8::MAX)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::UInt16(u16::MAX)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::UInt64(u64::MAX)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::Float32(1.5_f32)).unwrap();
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
        )
        .unwrap();
        let dt = chrono::DateTime::parse_from_rfc3339("2024-01-15T10:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let probe = SwitchKey::from_value(&Value::Timestamp(dt)).unwrap();
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
        )
        .unwrap();
        let probe = SwitchKey::from_value(&Value::Bytes(vec![0xde, 0xad, 0xbe, 0xef])).unwrap();
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
        )
        .unwrap_err();
        assert!(matches!(err, ValidationError::SwitchKeyTypeMismatch { .. }));
    }

    /// `truncate = true` with a sized `Text` sink shortens an RHS literal
    /// that exceeds the size — without it the underlying parser's
    /// `LengthExceeds` surfaces as `SwitchValueTypeMismatch`.
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
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));

        // With truncate: value is shortened to fit the declared size.
        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            true,
            &DataType::Int32,
            &sink,
            false,
        )
        .unwrap();
        let key = SwitchKey::from_value(&Value::Int32(1)).unwrap();
        assert_eq!(res.cases.get(&key), Some(&Value::Text("ALP".into())));
    }

    /// Bytes mirror of the Text truncate test: a sized `Bytes` sink
    /// with `truncate=true` shortens the parsed RHS bytes; without
    /// truncate the `LengthExceeds` error surfaces as
    /// `SwitchValueTypeMismatch`.
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
        )
        .unwrap_err();
        assert!(matches!(
            err,
            ValidationError::SwitchValueTypeMismatch { .. }
        ));

        let res = compile_switch(
            "f",
            "label",
            &cases,
            None,
            true,
            &DataType::Int32,
            &sink,
            false,
        )
        .unwrap();
        let key = SwitchKey::from_value(&Value::Int32(1)).unwrap();
        assert_eq!(res.cases.get(&key), Some(&Value::Bytes(vec![0xde, 0xad])));
    }
}
