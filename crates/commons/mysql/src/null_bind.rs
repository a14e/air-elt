//! Typed-NULL binding for sqlx MySQL.
//!
//! Same motivation as the pg counterpart: `Option::<T>::None` carries a wire
//! type that must match the column. Picking the right `None` per canonical
//! `DataType` keeps NULL inserts/comparisons working everywhere.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::mysql::{MySql, MySqlArguments};
use sqlx::query::Query;
use uuid::Uuid;

use air_elt_core::types::DataType;

pub fn bind_typed_null<'q>(
    query: Query<'q, MySql, MySqlArguments>,
    dt: DataType,
) -> Query<'q, MySql, MySqlArguments> {
    match dt {
        DataType::Bool => query.bind::<Option<bool>>(None),
        DataType::Int16 => query.bind::<Option<i16>>(None),
        DataType::Int32 => query.bind::<Option<i32>>(None),
        DataType::Int64 => query.bind::<Option<i64>>(None),
        DataType::Float32 => query.bind::<Option<f32>>(None),
        DataType::Float64 => query.bind::<Option<f64>>(None),
        DataType::Text { .. } => query.bind::<Option<String>>(None),
        DataType::Bytes { .. } => query.bind::<Option<Vec<u8>>>(None),
        DataType::Date => query.bind::<Option<NaiveDate>>(None),
        DataType::Timestamp => query.bind::<Option<DateTime<Utc>>>(None),
        // MySQL has no native UUID — we route uuid columns through Text or Bytes
        // before reaching this point. If it leaks through, fall back to Bytes(16).
        DataType::Uuid => query.bind::<Option<Uuid>>(None),
        DataType::Json => query.bind::<Option<serde_json::Value>>(None),
    }
}
