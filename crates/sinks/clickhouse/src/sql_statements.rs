//! SQL emitted by the ClickHouse sink.

use air_elt_commons_clickhouse::identifier::{quote_columns, quote_qualified};
use air_elt_core::error::RuntimeResult;

/// `INSERT INTO db.t (col1, col2, ...) FORMAT RowBinary` — the SQL
/// statement for an HTTP RowBinary body upload. CH parses this string
/// from the URL `query` parameter; the request body is purely the
/// per-row binary payload.
pub fn insert_row_binary_sql(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let qt = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {qt} ({cols}) FORMAT RowBinary"))
}

/// Zero-row write probe. CH evaluates the SELECT and validates the
/// INSERT column types but never writes a row.
pub fn probe_insert_where_false(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let qt = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!(
        "INSERT INTO {qt} ({cols}) SELECT {cols} FROM {qt} WHERE FALSE"
    ))
}
