use sqlx::PgPool;
use sqlx::prelude::FromRow;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Field, Schema};

use super::identifier::split_qualified;
use super::pg_type::{self, PgType};

/// Single `information_schema.columns` row. Named struct beats a wide tuple —
/// the type complexity gets out of hand once we add numeric precision/scale.
#[derive(FromRow)]
struct ColumnRow {
    column_name: String,
    is_nullable: String,
    udt_name: String,
    data_type: String,
    // CockroachDB returns these as INT8 (i64) where stock Postgres uses INT4
    // (i32). Casting to bigint in the SELECT normalises the wire type so the
    // same `Option<i64>` decode works against both engines.
    character_maximum_length: Option<i64>,
    numeric_precision: Option<i64>,
    numeric_scale: Option<i64>,
}

const INFORMATION_SCHEMA: &str = "SELECT column_name, is_nullable, udt_name, data_type, \
                                  character_maximum_length::bigint AS character_maximum_length, \
                                  numeric_precision::bigint AS numeric_precision, \
                                  numeric_scale::bigint AS numeric_scale \
    FROM information_schema.columns \
    WHERE table_schema = $1 AND table_name = $2 \
    ORDER BY ordinal_position";

pub async fn fetch_schema(pool: &PgPool, table: &str) -> RuntimeResult<Schema> {
    let (schema_name, table_name) = split_qualified(table)?;
    let rows: Vec<ColumnRow> = sqlx::query_as(INFORMATION_SCHEMA)
        .bind(&schema_name)
        .bind(&table_name)
        .fetch_all(pool)
        .await
        .map_err(RuntimeError::backend)?;

    if rows.is_empty() {
        return Err(RuntimeError::Other(format!(
            "table {schema_name:?}.{table_name:?} not found or not visible to current user"
        )));
    }

    let mut fields = Vec::with_capacity(rows.len());
    for row in rows {
        let pg: PgType = PgType::parse(&row.udt_name)
            .or_else(|| PgType::parse(&row.data_type))
            .ok_or_else(|| {
                RuntimeError::Other(format!(
                    "unsupported pg type for column {col:?}: udt={udt:?}, data_type={dt:?}",
                    col = row.column_name,
                    udt = row.udt_name,
                    dt = row.data_type,
                ))
            })?;
        let size = row
            .character_maximum_length
            .and_then(|n| u32::try_from(n).ok().filter(|v| *v > 0));
        let prec = row.numeric_precision.and_then(|n| u32::try_from(n).ok());
        let scale = row.numeric_scale.and_then(|n| u32::try_from(n).ok());
        fields.push(Field {
            name: row.column_name,
            data_type: pg_type::to_internal(pg, size, prec, scale),
            nullable: row.is_nullable.eq_ignore_ascii_case("YES"),
        });
    }
    Ok(Schema::new(fields))
}
