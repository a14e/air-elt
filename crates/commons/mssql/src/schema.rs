//! MS SQL table-schema introspection via `INFORMATION_SCHEMA.COLUMNS`.
//!
//! Uses tiberius directly — queries `INFORMATION_SCHEMA.COLUMNS` and
//! maps each column to a `Field` via `mssql_type::to_internal`.

use bb8::Pool;
use bb8_tiberius::ConnectionManager;
use tracing::warn;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Field, Schema};

use super::identifier::split_qualified;
use super::mssql_type;

const COLUMNS_QUERY: &str = "\
    SELECT \
        COLUMN_NAME, \
        IS_NULLABLE, \
        DATA_TYPE, \
        CHARACTER_MAXIMUM_LENGTH, \
        NUMERIC_PRECISION, \
        NUMERIC_SCALE \
    FROM INFORMATION_SCHEMA.COLUMNS \
    WHERE TABLE_SCHEMA = @P1 AND TABLE_NAME = @P2 \
    ORDER BY ORDINAL_POSITION";

pub async fn fetch_schema(pool: &Pool<ConnectionManager>, table: &str) -> RuntimeResult<Schema> {
    let (schema_name, table_name) = split_qualified(table)?;

    let mut conn = pool.get().await.map_err(RuntimeError::backend)?;

    let stream = conn
        .query(
            COLUMNS_QUERY,
            &[&schema_name.as_str(), &table_name.as_str()],
        )
        .await
        .map_err(RuntimeError::backend)?;

    let rows = stream
        .into_first_result()
        .await
        .map_err(RuntimeError::backend)?;

    if rows.is_empty() {
        return Err(RuntimeError::Other(format!(
            "table {schema_name:?}.{table_name:?} not found or not visible to current user"
        )));
    }

    let mut fields = Vec::with_capacity(rows.len());
    for row in rows {
        // Mandatory columns: fail loudly on decode errors instead of
        // silently producing empty/default values.
        let col_opt: Option<&str> = row
            .try_get::<&str, _>("COLUMN_NAME")
            .map_err(RuntimeError::backend)?;
        let col = col_opt
            .ok_or_else(|| {
                RuntimeError::Other("INFORMATION_SCHEMA.COLUMNS row missing COLUMN_NAME".into())
            })?
            .to_string();
        let is_null_opt: Option<&str> = row
            .try_get::<&str, _>("IS_NULLABLE")
            .map_err(RuntimeError::backend)?;
        let is_null = is_null_opt.ok_or_else(|| {
            RuntimeError::Other(format!(
                "INFORMATION_SCHEMA.COLUMNS row missing IS_NULLABLE for column {col:?}"
            ))
        })?;
        let data_type_opt: Option<&str> = row
            .try_get::<&str, _>("DATA_TYPE")
            .map_err(RuntimeError::backend)?;
        let data_type = data_type_opt.ok_or_else(|| {
            RuntimeError::Other(format!(
                "INFORMATION_SCHEMA.COLUMNS row missing DATA_TYPE for column {col:?}"
            ))
        })?;
        // Optional metadata columns — warn on decode failure but tolerate.
        let cml: Option<i32> = match row.try_get::<i32, _>("CHARACTER_MAXIMUM_LENGTH") {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, column = %col, "could not decode CHARACTER_MAXIMUM_LENGTH");
                None
            }
        };
        let np: Option<u8> = match row.try_get::<u8, _>("NUMERIC_PRECISION") {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, column = %col, "could not decode NUMERIC_PRECISION");
                None
            }
        };
        let ns: Option<i32> = match row.try_get::<i32, _>("NUMERIC_SCALE") {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, column = %col, "could not decode NUMERIC_SCALE");
                None
            }
        };

        let mssql = mssql_type::parse(data_type).ok_or_else(|| {
            RuntimeError::Other(format!(
                "unsupported mssql type for column {col:?}: data_type={data_type:?}"
            ))
        })?;
        let size = cml.and_then(|n| u32::try_from(n).ok());
        let prec = np.map(u32::from);
        let scale = ns.and_then(|n| u32::try_from(n).ok());
        fields.push(Field {
            name: col,
            data_type: mssql_type::to_internal(mssql, size, prec, scale),
            nullable: is_null.eq_ignore_ascii_case("YES"),
        });
    }
    Ok(Schema::new(fields))
}
