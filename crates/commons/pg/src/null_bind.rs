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
//! comparisons. Sink-side has its own helper at `commons-pg::sink_bind`
//! (`bind_value_separated`) which handles both NULL and non-NULL values
//! inside a `QueryBuilder::Separated` chain — shared between insert
//! (`push_values`) and delete (`push_tuples`) paths.

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::{PgArguments, Postgres};
use sqlx::query::Query;
use uuid::Uuid;

use air_elt_core::types::DataType;

use crate::types::PgHllType;

pub fn bind_typed_null<'q>(
    query: Query<'q, Postgres, PgArguments>,
    dt: DataType,
) -> Query<'q, Postgres, PgArguments> {
    match dt {
        DataType::Int64 => query.bind::<Option<i64>>(None),
        DataType::Bool => query.bind::<Option<bool>>(None),
        // Postgres has no int1 type; bind as int2 (i16).
        DataType::Int8 => query.bind::<Option<i16>>(None),
        DataType::Int16 => query.bind::<Option<i16>>(None),
        DataType::Int32 => query.bind::<Option<i32>>(None),
        DataType::Float32 => query.bind::<Option<f32>>(None),
        DataType::Float64 => query.bind::<Option<f64>>(None),
        // PG `numeric` accepts BigDecimal-typed NULLs for any (p, s) and
        // unbounded numeric — the OID is the same.
        DataType::BigInt { .. } | DataType::Decimal { .. } => {
            query.bind::<Option<BigDecimal>>(None)
        }
        // PG has no native unsigned integer types — these variants only
        // exist on the MySQL side. The runner reaches this null-bind path
        // only for source/sink columns that are *PG* columns, so unsigned
        // can never appear here. Defensive panic preserves exhaustiveness
        // without inventing a wire type.
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            unreachable!("postgres has no unsigned integer types")
        }
        DataType::Text { .. } => query.bind::<Option<String>>(None),
        DataType::Bytes { .. } => query.bind::<Option<Vec<u8>>>(None),
        DataType::Date => query.bind::<Option<NaiveDate>>(None),
        DataType::Timestamp => query.bind::<Option<DateTime<Utc>>>(None),
        DataType::Uuid => query.bind::<Option<Uuid>>(None),
        DataType::Json => query.bind::<Option<serde_json::Value>>(None),
        // Xml carries the canonical text payload over the wire. sqlx has no
        // native `xml` type — bind as text; Postgres accepts text-typed
        // NULLs into `xml` columns.
        DataType::Xml => query.bind::<Option<String>>(None),
        // Union never reaches a PG sink: schemaful sinks declare concrete
        // column types, and the validation pipeline rejects Union → PG
        // before any bind happens.
        DataType::Union(_) => unreachable!("postgres sinks never carry Union types"),
        // HLL: bind a typed NULL `Vec<u8>` (matches the `bytea`-shaped
        // wire encoding sqlx uses for HLL bytes). The `::hll` cast is
        // emitted by the SQL template builder rather than here — this
        // helper has no access to the surrounding SQL fragment. The
        // cursor path never reaches this arm because HLL has
        // `cursor_compatible() == false`; this is the safety net for any
        // future write/cursor code that null-binds HLL.
        DataType::Custom(t) if t.kind() == PgHllType::KIND => query.bind::<Option<Vec<u8>>>(None),
        // Other custom types are connector-specific and need a
        // dedicated bind path. Reaching this arm means the matrix /
        // validation guards let through a custom type with no codec
        // wired in — a structural bug, not a runtime data shape.
        DataType::Custom(_) => unreachable!(
            "DataType::Custom must be handled by the connector before reaching null_bind"
        ),
    }
}
