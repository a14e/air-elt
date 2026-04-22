//! Sink-side: imports the shared `PgType` / `to_internal` from commons, and
//! owns the reverse `from_internal` (canonical `DataType` → `PgType`) which
//! is sink-specific — it picks which pg type we expect *for that canonical
//! slot*. Timestamp→timestamptz is deliberate: our canonical `Timestamp` is
//! UTC-normalised, timestamptz is the matching pg shape.

pub use air_elt_commons::sql::pg::pg_type::{PgType, to_internal};

use air_elt_core::error::TypeError;
use air_elt_core::types::DataType;

pub fn from_internal(dt: DataType, column: &str) -> Result<PgType, TypeError> {
    Ok(match dt {
        // Why NullSinkColumn: canonical `Null` has no pg representation. This
        // arm can only fire if the sink schema *itself* declares a Null column
        // — impossible today since introspection never returns Null. The
        // explicit variant is for future-proofing and readable errors.
        DataType::Null => {
            return Err(TypeError::NullSinkColumn {
                column: column.to_string(),
            });
        }
        DataType::Bool => PgType::Bool,
        DataType::Int16 => PgType::Int2,
        DataType::Int32 => PgType::Int4,
        DataType::Int64 => PgType::Int8,
        DataType::Float32 => PgType::Float4,
        DataType::Float64 => PgType::Float8,
        DataType::Text => PgType::Text,
        DataType::Bytes => PgType::Bytea,
        DataType::Date => PgType::Date,
        DataType::Timestamp => PgType::TimestampTz,
        DataType::Uuid => PgType::Uuid,
        DataType::Json => PgType::Jsonb,
    })
}
