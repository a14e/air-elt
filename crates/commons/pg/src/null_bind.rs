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

use crate::types::{PgHllType, PgInetType};

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
        DataType::Ipv4 | DataType::Ipv6 => {
            query.bind::<Option<sqlx::types::ipnetwork::IpNetwork>>(None)
        }
        DataType::Json => query.bind::<Option<serde_json::Value>>(None),
        // Xml carries the canonical text payload over the wire. sqlx has no
        // native `xml` type — bind as text; Postgres accepts text-typed
        // NULLs into `xml` columns.
        DataType::Xml => query.bind::<Option<String>>(None),
        // Union never reaches a PG sink: schemaful sinks declare concrete
        // column types, and the validation pipeline rejects Union → PG
        // before any bind happens.
        // Object is handled as Json in connectors
        DataType::Object => query.bind::<Option<serde_json::Value>>(None),
        // Native array NULL: bind `None::<Vec<Option<NativeT>>>` for the
        // element type so the wire OID is the matching array OID. Without
        // the right element type sqlx would emit the wrong array OID and
        // PG would reject the typed NULL against the column.
        DataType::Array { element, .. } => bind_typed_null_array(query, element.as_deref()),
        DataType::Union(_) => unreachable!("postgres sinks never carry Union types"),
        // Interval is a redis-only canonical type; a PG sink never declares
        // an Interval column, and the matrix rejects `* → Interval` for any
        // other pair, so this null-bind path is never reached.
        DataType::Interval => unreachable!("postgres sinks never carry Interval (redis-only type)"),
        // HLL: bind a typed NULL `Vec<u8>` (matches the `bytea`-shaped
        // wire encoding sqlx uses for HLL bytes). The `::hll` cast is
        // emitted by the SQL template builder rather than here — this
        // helper has no access to the surrounding SQL fragment. The
        // cursor path never reaches this arm because HLL has
        // `cursor_compatible() == false`; this is the safety net for any
        // future write/cursor code that null-binds HLL.
        DataType::Custom(t) if t.kind() == PgHllType::KIND => query.bind::<Option<Vec<u8>>>(None),
        // PG `inet` typed NULL: sqlx-postgres recognises
        // `Option::<IpNetwork>::None` against the `inet` OID directly.
        DataType::Custom(t) if t.kind() == PgInetType::KIND => {
            query.bind::<Option<sqlx::types::ipnetwork::IpNetwork>>(None)
        }
        // Other custom types are connector-specific and need a
        // dedicated bind path. Reaching this arm means the matrix /
        // validation guards let through a custom type with no codec
        // wired in — a structural bug, not a runtime data shape.
        DataType::Custom(_) => unreachable!(
            "DataType::Custom must be handled by the connector before reaching null_bind"
        ),
    }
}

/// Bind a typed NULL for a native PG array column, choosing the
/// `Vec<Option<NativeT>>` whose element OID matches `element`.
///
/// `element == None` means the array element type is empty/unknown — only
/// the expression / source layer produces that shape; a PG *column* always
/// declares a concrete element. We bind a `text[]` NULL as the
/// universally-castable fallback so an unexpected unknown-element column
/// still lands a valid NULL rather than panicking.
fn bind_typed_null_array<'q>(
    query: Query<'q, Postgres, PgArguments>,
    element: Option<&DataType>,
) -> Query<'q, Postgres, PgArguments> {
    match element {
        Some(DataType::Bool) => query.bind::<Option<Vec<Option<bool>>>>(None),
        // Postgres has no int1 type; bind int1 elements as int2[] (i16).
        Some(DataType::Int8 | DataType::Int16) => query.bind::<Option<Vec<Option<i16>>>>(None),
        Some(DataType::Int32) => query.bind::<Option<Vec<Option<i32>>>>(None),
        Some(DataType::Int64) => query.bind::<Option<Vec<Option<i64>>>>(None),
        Some(DataType::Float32) => query.bind::<Option<Vec<Option<f32>>>>(None),
        Some(DataType::Float64) => query.bind::<Option<Vec<Option<f64>>>>(None),
        Some(DataType::Text { .. }) => query.bind::<Option<Vec<Option<String>>>>(None),
        Some(DataType::Date) => query.bind::<Option<Vec<Option<NaiveDate>>>>(None),
        Some(DataType::Timestamp) => query.bind::<Option<Vec<Option<DateTime<Utc>>>>>(None),
        Some(DataType::Uuid) => query.bind::<Option<Vec<Option<Uuid>>>>(None),
        // numeric[] — BigInt and Decimal both map to the `numeric` OID.
        Some(DataType::BigInt { .. } | DataType::Decimal { .. }) => {
            query.bind::<Option<Vec<Option<BigDecimal>>>>(None)
        }
        // Fallback for the unknown-element case (see fn docstring).
        None => query.bind::<Option<Vec<Option<String>>>>(None),
        // Any other element type was rejected by the native-type mapper
        // (`PgType::is_array_element`), so a PG array column never declares
        // it. Reaching here means a non-PG-derived array type slipped past
        // validation — a structural bug, not a runtime data shape.
        Some(other) => {
            unreachable!("postgres array column declared unsupported element type {other:?}")
        }
    }
}
