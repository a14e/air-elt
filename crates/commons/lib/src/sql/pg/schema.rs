use sqlx::PgPool;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Field, Schema};

use super::identifier::split_qualified;
use super::pg_type::{self, PgType};

const INFORMATION_SCHEMA: &str = "SELECT column_name, is_nullable, udt_name, data_type
    FROM information_schema.columns
    WHERE table_schema = $1 AND table_name = $2
    ORDER BY ordinal_position";

pub async fn fetch_schema(pool: &PgPool, table: &str) -> RuntimeResult<Schema> {
    let (schema_name, table_name) = split_qualified(table);
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(INFORMATION_SCHEMA)
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
    for (col, is_null, udt, data_type) in rows {
        let pg: PgType = PgType::parse(&udt)
            .or_else(|| PgType::parse(&data_type))
            .ok_or_else(|| {
                RuntimeError::Other(format!(
                    "unsupported pg type for column {col:?}: udt={udt:?}, data_type={data_type:?}"
                ))
            })?;
        fields.push(Field {
            name: col,
            data_type: pg_type::to_internal(pg),
            nullable: is_null.eq_ignore_ascii_case("YES"),
        });
    }
    Ok(Schema::new(fields))
}
