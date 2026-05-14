//! SQL emitted by the QuestDB sink's pg-wire writer.
//!
//! All identifier interpolation goes through
//! `air_elt_commons_questdb::identifier::{quote_qualified, quote_columns}` —
//! never inline a name in `format!` here.

use air_elt_commons_questdb::identifier::{quote_columns, quote_qualified};
use air_elt_core::error::RuntimeResult;

/// `INSERT INTO "<table>" ("c1","c2",...) ` — the prefix passed into a
/// sqlx `QueryBuilder::push_values` chain. The caller appends the
/// `VALUES (...), (...)` tail.
pub fn insert_sql_pg(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let qt = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {qt} ({cols}) "))
}

/// `INSERT INTO "<table>" ("c1",...) SELECT $1, $2, ..., $N FROM long_sequence(0)`
/// — the never-produces-a-row dry-run statement.
///
/// QuestDB's planner walks the column list, type-checks each bind parameter
/// against the target column, validates table+column existence and
/// permissions, then ranges over `long_sequence(0)` (zero rows) so no row
/// reaches the writer. No transaction, no rollback risk, no sentinel
/// timestamp. The bare-`SELECT $1 WHERE 1=0` form is rejected by QuestDB's
/// pg-wire planner ("table and column names that are SQL keywords ..."),
/// so we ground the SELECT in QuestDB's standard zero-row generator.
pub fn dry_run_sql_pg(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let qt = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    let mut placeholders = String::new();
    for i in 0..columns.len() {
        if i > 0 {
            placeholders.push_str(", ");
        }
        placeholders.push('$');
        placeholders.push_str(&(i + 1).to_string());
    }
    Ok(format!(
        "INSERT INTO {qt} ({cols}) SELECT {placeholders} FROM long_sequence(0)"
    ))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn insert_sql_basic() {
        let sql = insert_sql_pg("metrics", &["ts".into(), "value".into()]).unwrap();
        assert_eq!(sql, "INSERT INTO \"metrics\" (\"ts\", \"value\") ");
    }

    #[test]
    fn dry_run_sql_basic() {
        let sql = dry_run_sql_pg("metrics", &["ts".into(), "value".into()]).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"metrics\" (\"ts\", \"value\") SELECT $1, $2 FROM long_sequence(0)"
        );
    }

    #[test]
    fn dry_run_sql_single_column() {
        let sql = dry_run_sql_pg("t", &["c".into()]).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"t\" (\"c\") SELECT $1 FROM long_sequence(0)"
        );
    }
}
