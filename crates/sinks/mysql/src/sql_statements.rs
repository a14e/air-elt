//! SQL emitted by the MySQL sink.

use air_elt_commons_mysql::identifier::{quote_columns, quote_qualified};
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

/// INSERT statement consumed by `QueryBuilder::push_values`; caller appends
/// `VALUES ...`. MySQL syntax is identical to pg here.
pub fn insert_statement(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {quoted_table} ({cols}) "))
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
}
