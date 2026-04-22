//! SQL emitted by the sink.

pub use air_elt_commons::sql::pg::identifier::split_qualified;
use air_elt_commons::sql::pg::identifier::{quote_columns, quote_qualified};
use air_elt_core::error::RuntimeResult;

pub const PING: &str = "SELECT 1";

pub const INFORMATION_SCHEMA: &str = "SELECT column_name, is_nullable, udt_name, data_type
    FROM information_schema.columns
    WHERE table_schema = $1 AND table_name = $2
    ORDER BY ordinal_position";

pub const HAS_TABLE_INSERT: &str = "SELECT has_table_privilege(current_user, $1, 'INSERT') AS ok";

/// `INSERT INTO "schema"."t" ("c1","c2") SELECT "c1","c2" FROM "schema"."t" WHERE false`
pub fn probe_insert_where_false(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!(
        "INSERT INTO {quoted_table} ({cols}) \
         SELECT {cols} FROM {quoted_table} WHERE false"
    ))
}

/// Prefix consumed by `QueryBuilder::push_values`; caller appends `VALUES ...`.
pub fn insert_prefix(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {quoted_table} ({cols}) "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_insert_form() {
        let sql = probe_insert_where_false("public.users", &["id".into(), "name".into()]).unwrap();
        assert_eq!(
            sql,
            "INSERT INTO \"public\".\"users\" (\"id\", \"name\") \
             SELECT \"id\", \"name\" FROM \"public\".\"users\" WHERE false"
        );
    }

    #[test]
    fn insert_prefix_form() {
        let sql = insert_prefix("analytics.events", &["event_id".into()]).unwrap();
        assert_eq!(sql, "INSERT INTO \"analytics\".\"events\" (\"event_id\") ");
    }
}
