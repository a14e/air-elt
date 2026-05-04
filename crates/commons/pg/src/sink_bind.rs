//! Postgres sink-side `Separated` binding.
//!
//! Used by:
//! * the insert path inside `push_values` lambda
//! * the DELETE path inside `push_tuples` lambda
//! * the single-key DELETE path that builds a flat `IN (...)` list via
//!   `QueryBuilder::separated`
//!
//! All three call into a `Separated`-shaped binder. The arms are the
//! same as `null_bind::bind_typed_null` (which targets a `Query`); the
//! lifetimes diverge enough that the two cannot be merged generically
//! without either pulling in HKT or boxing the binder.
//!
//! Keep the two arm sets in lockstep — adding a `DataType` variant
//! must update both files.

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::Postgres;
use sqlx::query_builder::Separated;
use uuid::Uuid;

use air_elt_core::types::{DataType, Value};

pub fn bind_value_separated(sep: &mut Separated<'_, '_, Postgres, &str>, v: &Value, dt: &DataType) {
    match v {
        Value::Null => match dt {
            DataType::Int64 => {
                sep.push_bind::<Option<i64>>(None);
            }
            DataType::Bool => {
                sep.push_bind::<Option<bool>>(None);
            }
            DataType::Int16 => {
                sep.push_bind::<Option<i16>>(None);
            }
            DataType::Int32 => {
                sep.push_bind::<Option<i32>>(None);
            }
            DataType::Float32 => {
                sep.push_bind::<Option<f32>>(None);
            }
            DataType::Float64 => {
                sep.push_bind::<Option<f64>>(None);
            }
            DataType::Text { .. } => {
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Bytes { .. } => {
                sep.push_bind::<Option<Vec<u8>>>(None);
            }
            DataType::Date => {
                sep.push_bind::<Option<NaiveDate>>(None);
            }
            DataType::Timestamp => {
                sep.push_bind::<Option<DateTime<Utc>>>(None);
            }
            DataType::Uuid => {
                sep.push_bind::<Option<Uuid>>(None);
            }
            DataType::Json => {
                sep.push_bind::<Option<serde_json::Value>>(None);
            }
            DataType::BigInt { .. } | DataType::Decimal { .. } => {
                sep.push_bind::<Option<BigDecimal>>(None);
            }
            DataType::Xml => {
                sep.push_bind::<Option<String>>(None);
            }
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                unreachable!("postgres has no unsigned integer column types")
            }
            DataType::Union(_) => unreachable!("postgres sinks never carry Union types"),
        },
        Value::Bool(b) => {
            sep.push_bind(*b);
        }
        Value::Int16(n) => {
            sep.push_bind(*n);
        }
        Value::Int32(n) => {
            sep.push_bind(*n);
        }
        Value::Int64(n) => {
            sep.push_bind(*n);
        }
        Value::Float32(n) => {
            sep.push_bind(*n);
        }
        Value::Float64(n) => {
            sep.push_bind(*n);
        }
        Value::Text(s) => {
            sep.push_bind(s.clone());
        }
        Value::Bytes(b) => {
            sep.push_bind(b.clone());
        }
        Value::Date(d) => {
            sep.push_bind(*d);
        }
        Value::Timestamp(ts) => {
            sep.push_bind(*ts);
        }
        Value::Uuid(u) => {
            sep.push_bind(*u);
        }
        Value::Json(j) => {
            sep.push_bind(j.clone());
        }
        Value::BigInt(b) => {
            // Lift BigInt to BigDecimal scale 0 — sqlx pg numeric only encodes via BigDecimal.
            sep.push_bind(BigDecimal::new(b.clone(), 0));
        }
        Value::Decimal(d) => {
            sep.push_bind(d.clone());
        }
        Value::UInt8(_) | Value::UInt16(_) | Value::UInt32(_) | Value::UInt64(_) => {
            unreachable!(
                "unsigned values cannot reach a postgres sink: pg schemas never \
                 produce UInt* (no native unsigned int columns), and the convert \
                 dispatcher rewrites every UInt → Int*/BigInt/Decimal mapping into \
                 the target variant before binding"
            )
        }
    }
}
