//! MySQL table-schema introspection via `information_schema.COLUMNS`.

use sqlx::MySqlPool;
use sqlx::Row;

use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::{Field, Schema};

use super::identifier::split_qualified;
use super::mysql_type;

// MySQL 8 returns INFORMATION_SCHEMA column names in upper case and the
// underlying string columns as `BLOB` (utf8mb3 system collation). Aliasing
// fixes the case; explicit CAST(... AS CHAR) makes them decode cleanly as
// `String` via sqlx.
const COLUMNS_QUERY: &str = "SELECT \
        CAST(column_name AS CHAR)  AS column_name, \
        CAST(is_nullable AS CHAR)  AS is_nullable, \
        CAST(data_type AS CHAR)    AS data_type, \
        CAST(column_type AS CHAR)  AS column_type, \
        character_maximum_length   AS character_maximum_length, \
        numeric_precision          AS numeric_precision, \
        numeric_scale              AS numeric_scale \
    FROM information_schema.columns \
    WHERE table_schema = ? AND table_name = ? \
    ORDER BY ordinal_position";

pub async fn fetch_schema(pool: &MySqlPool, table: &str) -> RuntimeResult<Schema> {
    let (db_opt, table_name) = split_qualified(table)?;
    let db = match db_opt {
        Some(db) => db,
        None => current_database(pool).await?,
    };

    // CHARACTER_MAXIMUM_LENGTH is `BIGINT` in MySQL 8.0+ (it was unsigned in
    // older versions); we read it as `i64` and convert to `u32` afterwards.
    // Negative values are not expected per the spec.
    let rows = sqlx::query(COLUMNS_QUERY)
        .bind(&db)
        .bind(&table_name)
        .fetch_all(pool)
        .await
        .map_err(RuntimeError::backend)?;

    if rows.is_empty() {
        return Err(RuntimeError::Other(format!(
            "table {db:?}.{table_name:?} not found or not visible to current user"
        )));
    }

    let mut fields = Vec::with_capacity(rows.len());
    for row in rows {
        let col: String = row.try_get("column_name").map_err(RuntimeError::backend)?;
        let is_null: String = row.try_get("is_nullable").map_err(RuntimeError::backend)?;
        let data_type: String = row.try_get("data_type").map_err(RuntimeError::backend)?;
        let column_type: String = row.try_get("column_type").map_err(RuntimeError::backend)?;
        let cml: Option<i64> = row
            .try_get("character_maximum_length")
            .map_err(RuntimeError::backend)?;
        // numeric_precision / numeric_scale are `BIGINT UNSIGNED` in MySQL 8
        // (vs character_maximum_length which is signed BIGINT — see above).
        let np: Option<u64> = row
            .try_get("numeric_precision")
            .map_err(RuntimeError::backend)?;
        let ns: Option<u64> = row
            .try_get("numeric_scale")
            .map_err(RuntimeError::backend)?;

        let mysql = mysql_type::parse(&data_type, &column_type).ok_or_else(|| {
            RuntimeError::Other(format!(
                "unsupported mysql type for column {col:?}: data_type={data_type:?}, \
                 column_type={column_type:?}"
            ))
        })?;
        let size = cml.and_then(|n| u32::try_from(n).ok());
        let prec = np.and_then(|n| u32::try_from(n).ok());
        let scale = ns.and_then(|n| u32::try_from(n).ok());
        fields.push(Field {
            name: col,
            data_type: mysql_type::to_internal(mysql, size, prec, scale),
            nullable: is_null.eq_ignore_ascii_case("YES"),
        });
    }
    Ok(Schema::new(fields))
}

async fn current_database(pool: &MySqlPool) -> RuntimeResult<String> {
    let row = sqlx::query("SELECT DATABASE() AS db")
        .fetch_one(pool)
        .await
        .map_err(RuntimeError::backend)?;
    let db: Option<String> = row.try_get("db").map_err(RuntimeError::backend)?;
    db.ok_or_else(|| {
        RuntimeError::Other(
            "no database selected — connection URL must include the database name or table \
             must be schema-qualified (e.g. `appdb.users`)"
                .to_string(),
        )
    })
}
