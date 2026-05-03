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

    // CHARACTER_MAXIMUM_LENGTH surfaces with different signedness on
    // different servers (MySQL 8 reports it signed, MariaDB unsigned).
    // sqlx's strict type check rejects the wrong half — try unsigned
    // first (the natural representation), then fall back to signed.
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
        let cml: Option<u64> = match row.try_get::<Option<u64>, _>("character_maximum_length") {
            Ok(v) => v,
            Err(_) => row
                .try_get::<Option<i64>, _>("character_maximum_length")
                .map_err(RuntimeError::backend)?
                .and_then(|n| u64::try_from(n).ok()),
        };
        // numeric_precision / numeric_scale also vary in signedness
        // across MySQL / MariaDB releases — same fallback pattern.
        let np: Option<u64> = match row.try_get::<Option<u64>, _>("numeric_precision") {
            Ok(v) => v,
            Err(_) => row
                .try_get::<Option<i64>, _>("numeric_precision")
                .map_err(RuntimeError::backend)?
                .and_then(|n| u64::try_from(n).ok()),
        };
        let ns: Option<u64> = match row.try_get::<Option<u64>, _>("numeric_scale") {
            Ok(v) => v,
            Err(_) => row
                .try_get::<Option<i64>, _>("numeric_scale")
                .map_err(RuntimeError::backend)?
                .and_then(|n| u64::try_from(n).ok()),
        };

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
