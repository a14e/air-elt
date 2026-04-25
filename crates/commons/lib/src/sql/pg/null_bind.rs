//! Typed-NULL binding for sqlx Postgres.
//!
//! Why this module exists: sqlx's wire protocol carries a type OID with every
//! bind, including `Option::<T>::None` (it uses T's OID). Binding
//! `Option::<i64>::None` against a `timestamptz` / `uuid` / `jsonb` column
//! fails with a server-side type-mismatch. The helpers below choose the
//! right `None` variant per canonical `DataType` so NULLs land correctly
//! on *any* column.
//!
//! Source-side uses `bind_typed_null` on a `sqlx::query::Query` for cursor
//! comparisons. Sink-side inlines the same match inside `push_values` because
//! the `Separated` lifetime prevents extracting it into a helper.

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::{PgArguments, Postgres};
use sqlx::query::Query;
use uuid::Uuid;

use air_elt_core::types::DataType;

pub fn bind_typed_null<'q>(
    query: Query<'q, Postgres, PgArguments>,
    dt: DataType,
) -> Query<'q, Postgres, PgArguments> {
    match dt {
        DataType::Int64 => query.bind::<Option<i64>>(None),
        DataType::Bool => query.bind::<Option<bool>>(None),
        DataType::Int16 => query.bind::<Option<i16>>(None),
        DataType::Int32 => query.bind::<Option<i32>>(None),
        DataType::Float32 => query.bind::<Option<f32>>(None),
        DataType::Float64 => query.bind::<Option<f64>>(None),
        DataType::Text => query.bind::<Option<String>>(None),
        DataType::Bytes => query.bind::<Option<Vec<u8>>>(None),
        DataType::Date => query.bind::<Option<NaiveDate>>(None),
        DataType::Timestamp => query.bind::<Option<DateTime<Utc>>>(None),
        DataType::Uuid => query.bind::<Option<Uuid>>(None),
        DataType::Json => query.bind::<Option<serde_json::Value>>(None),
    }
}
