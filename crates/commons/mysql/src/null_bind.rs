//! Typed-NULL binding for sqlx MySQL.
//!
//! Same motivation as the pg counterpart: `Option::<T>::None` carries a wire
//! type that must match the column. Picking the right `None` per canonical
//! `DataType` keeps NULL inserts/comparisons working everywhere.

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::mysql::{MySql, MySqlArguments};
use sqlx::query::Query;

use air_elt_core::types::DataType;

pub fn bind_typed_null<'q>(
    query: Query<'q, MySql, MySqlArguments>,
    dt: DataType,
) -> Query<'q, MySql, MySqlArguments> {
    match dt {
        DataType::Bool => query.bind::<Option<bool>>(None),
        DataType::Int8 => query.bind::<Option<i8>>(None),
        DataType::Int16 => query.bind::<Option<i16>>(None),
        DataType::Int32 => query.bind::<Option<i32>>(None),
        DataType::Int64 => query.bind::<Option<i64>>(None),
        DataType::Float32 => query.bind::<Option<f32>>(None),
        DataType::Float64 => query.bind::<Option<f64>>(None),
        DataType::Text { .. } => query.bind::<Option<String>>(None),
        DataType::Bytes { .. } => query.bind::<Option<Vec<u8>>>(None),
        DataType::Date => query.bind::<Option<NaiveDate>>(None),
        DataType::Timestamp => query.bind::<Option<DateTime<Utc>>>(None),
        // MariaDB 10.7+ native UUID column accepts text input reliably
        // (binary input triggers an internal byte-shuffle — see codec /
        // sink). NULLs are bound as `Option<String>` to keep the wire-type
        // OID consistent with the non-null path which sends canonical text.
        DataType::Uuid => query.bind::<Option<String>>(None),
        // Canonical IPv4/IPv6 bind as VARCHAR-style text NULLs.
        DataType::Ipv4 | DataType::Ipv6 => query.bind::<Option<String>>(None),
        DataType::Json => query.bind::<Option<serde_json::Value>>(None),
        DataType::BigInt { .. } | DataType::Decimal { .. } => {
            query.bind::<Option<BigDecimal>>(None)
        }
        DataType::UInt8 => query.bind::<Option<u8>>(None),
        DataType::UInt16 => query.bind::<Option<u16>>(None),
        DataType::UInt32 => query.bind::<Option<u32>>(None),
        DataType::UInt64 => query.bind::<Option<u64>>(None),
        // MySQL has no native xml type. Operators carrying canonical-text
        // XML through MySQL columns map them to text-family columns, so
        // this null-bind reaches the same wire type as Text.
        DataType::Xml => query.bind::<Option<String>>(None),
        // Union never reaches a MySQL sink: schemaful sinks declare
        // concrete column types, and validation rejects Union → MySQL
        // before any bind happens.
        // Object is handled as Json in connectors
        DataType::Object => query.bind::<Option<serde_json::Value>>(None),
        DataType::Union(_) => unreachable!("mysql sinks never carry Union types"),
        DataType::Interval => unreachable!("mysql sinks never carry Interval (redis-only type)"),
        DataType::Custom(_) => unreachable!(
            "DataType::Custom must be handled by the connector before reaching null_bind"
        ),
    }
}
