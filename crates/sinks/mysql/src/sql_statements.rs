//! SQL emitted by the MySQL sink.

use air_elt_commons_mysql::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::error::RuntimeResult;

pub const PING: &str = "SELECT 1";

/// Zero-row probe: the planner validates INSERT privilege and column types
/// without inserting anything. `WHERE FALSE` is portable mysql syntax.
pub fn probe_insert_where_false(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!(
        "INSERT INTO {quoted_table} ({cols}) \
         SELECT {cols} FROM {quoted_table} WHERE FALSE"
    ))
}

/// Zero-row probe of the DELETE path. MySQL/MariaDB has no
/// `has_table_privilege()` analogue, so we lean on the rolled-back
/// transaction to surface privilege / table-visibility errors.
pub fn probe_delete_where_false(table: &str) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    Ok(format!("DELETE FROM {quoted_table} WHERE FALSE"))
}

/// INSERT statement consumed by `QueryBuilder::push_values`; caller appends
/// `VALUES ...`. MySQL syntax is identical to pg here.
pub fn insert_statement(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {quoted_table} ({cols}) "))
}

/// `DELETE FROM `db`.`t` WHERE (`k1`,`k2`) IN (` — caller appends a
/// `push_tuples` loop that pushes the tuple values, then `)`.
pub fn delete_in_prefix(table: &str, key_columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let key_quoted = quote_columns(key_columns)?;
    if key_columns.len() == 1 {
        Ok(format!(
            "DELETE FROM {quoted_table} WHERE {key_quoted} IN ("
        ))
    } else {
        Ok(format!(
            "DELETE FROM {quoted_table} WHERE ({key_quoted}) IN ("
        ))
    }
}

/// Like `insert_statement` but for `ConflictStrategy::Ignore` — uses
/// `INSERT IGNORE` which silently drops rows that would violate any
/// unique constraint (the behaviour MySQL/MariaDB give us; we accept
/// it for opt-in operators).
pub fn insert_ignore_statement(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT IGNORE INTO {quoted_table} ({cols}) "))
}

/// Suffix appended after `VALUES (...)` for the `Overwrite` strategy.
/// Uses the legacy `VALUES()` form so MariaDB 10.x and old MySQL builds
/// stay supported (project rule: MariaDB is a test target). Returns
/// the empty string for `Ignore` because the prefix already encodes it.
pub fn conflict_overwrite_suffix(
    conflict: &ConflictConfig,
    all_columns: &[String],
) -> RuntimeResult<String> {
    if conflict.strategy != ConflictStrategy::Overwrite {
        return Ok(String::new());
    }
    let updates: Vec<String> = all_columns
        .iter()
        .filter(|c| !conflict.key.contains(c))
        .map(|c| {
            let q = quote_ident(c);
            format!("{q} = VALUES({q})")
        })
        .collect();
    if updates.is_empty() {
        // Every column is part of the key — VALUES() with no SET list
        // is invalid, and re-writing the same key is a no-op anyway.
        // Fall through to a guard that keeps the existing row.
        let key = conflict
            .key
            .first()
            .map(|s| quote_ident(s))
            .unwrap_or_else(|| "NULL".into());
        return Ok(format!(" ON DUPLICATE KEY UPDATE {key} = {key}"));
    }
    Ok(format!(" ON DUPLICATE KEY UPDATE {}", updates.join(", ")))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn probe_insert_form() {
        let sql = probe_insert_where_false("appdb.users", &["id".into(), "name".into()]).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO `appdb`.`users` (`id`, `name`) \
             SELECT `id`, `name` FROM `appdb`.`users` WHERE FALSE"
        );
    }

    #[test]
    fn insert_statement_form() {
        let sql = insert_statement("appdb.events", &["event_id".into()]).unwrap();
        assert_eq!(sql, "INSERT INTO `appdb`.`events` (`event_id`) ");
    }

    #[test]
    fn insert_ignore_form() {
        let sql = insert_ignore_statement("appdb.events", &["event_id".into()]).unwrap();
        assert_eq!(sql, "INSERT IGNORE INTO `appdb`.`events` (`event_id`) ");
    }

    #[test]
    fn overwrite_suffix() {
        let cfg = ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        };
        let s = conflict_overwrite_suffix(&cfg, &["id".into(), "name".into()]).unwrap();
        assert_eq!(s, " ON DUPLICATE KEY UPDATE `name` = VALUES(`name`)");
    }

    #[test]
    fn overwrite_suffix_ignore_returns_empty() {
        let cfg = ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Ignore,
        };
        let s = conflict_overwrite_suffix(&cfg, &["id".into(), "name".into()]).unwrap();
        assert_eq!(s, "");
    }
}
