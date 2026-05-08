//! MySQL sink-side `Separated` binding.
//!
//! Mirror of `commons-pg::sink_bind` for the mysql sink. Used by both
//! the insert path (`push_values`) and the DELETE path (`push_tuples`
//! / single-key `separated(", ")`). Keep the arms in lockstep with
//! `null_bind::bind_typed_null` (which targets a `Query`).

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::MySql;
use sqlx::query_builder::Separated;

use air_elt_core::types::{DataType, Value};

pub fn bind_value_separated(sep: &mut Separated<'_, '_, MySql, &str>, v: &Value, dt: &DataType) {
    match v {
        Value::Null => match dt {
            DataType::Bool => {
                sep.push_bind::<Option<bool>>(None);
            }
            DataType::Int16 => {
                sep.push_bind::<Option<i16>>(None);
            }
            DataType::Int32 => {
                sep.push_bind::<Option<i32>>(None);
            }
            DataType::Int64 => {
                sep.push_bind::<Option<i64>>(None);
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
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Json => {
                sep.push_bind::<Option<serde_json::Value>>(None);
            }
            DataType::BigInt { .. } | DataType::Decimal { .. } => {
                sep.push_bind::<Option<BigDecimal>>(None);
            }
            DataType::UInt8 => {
                sep.push_bind::<Option<u8>>(None);
            }
            DataType::UInt16 => {
                sep.push_bind::<Option<u16>>(None);
            }
            DataType::UInt32 => {
                sep.push_bind::<Option<u32>>(None);
            }
            DataType::UInt64 => {
                sep.push_bind::<Option<u64>>(None);
            }
            DataType::Xml => {
                sep.push_bind::<Option<String>>(None);
            }
            DataType::Union(_) => unreachable!("mysql sinks never carry Union types"),
            DataType::Custom(_) => unreachable!(
                "DataType::Custom must be handled by the connector before reaching sink_bind"
            ),
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
            sep.push_bind(u.to_string());
        }
        Value::Json(j) => {
            sep.push_bind(j.clone());
        }
        Value::BigInt(b) => {
            sep.push_bind(BigDecimal::new(b.clone(), 0));
        }
        Value::Decimal(d) => {
            sep.push_bind(d.clone());
        }
        Value::UInt8(n) => {
            sep.push_bind(*n);
        }
        Value::UInt16(n) => {
            sep.push_bind(*n);
        }
        Value::UInt32(n) => {
            sep.push_bind(*n);
        }
        Value::UInt64(n) => {
            sep.push_bind(*n);
        }
        Value::Custom(_) => {
            unreachable!("Value::Custom must be handled by the connector before reaching sink_bind")
        }
    }
}
