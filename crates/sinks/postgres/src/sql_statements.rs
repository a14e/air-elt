//! SQL emitted by the sink.

use air_elt_commons_pg::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::error::RuntimeResult;

pub const PING: &str = "SELECT 1";

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

/// INSERT statement consumed by `QueryBuilder::push_values`; caller appends `VALUES ...`.
pub fn insert_statement(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {quoted_table} ({cols}) "))
}

/// Suffix appended after `VALUES (...)` to translate the operator's
/// `[flow.<name>.conflict]` block into PG syntax.
/// - `Ignore` → `ON CONFLICT (k1,k2) DO NOTHING`
/// - `Overwrite` → `ON CONFLICT (k1,k2) DO UPDATE SET c=EXCLUDED.c, ...`
///
/// The non-key columns become the `SET` list; the key columns are
/// excluded from the update because writing the same key over itself
/// is a no-op the planner would otherwise have to filter out.
pub fn conflict_suffix(conflict: &ConflictConfig, all_columns: &[String]) -> RuntimeResult<String> {
    let key_quoted = quote_columns(&conflict.key)?;
    Ok(match conflict.strategy {
        ConflictStrategy::Ignore => format!(" ON CONFLICT ({key_quoted}) DO NOTHING"),
        ConflictStrategy::Overwrite => {
            let updates: Vec<String> = all_columns
                .iter()
                .filter(|c| !conflict.key.contains(c))
                .map(|c| {
                    let q = quote_ident(c);
                    format!("{q} = EXCLUDED.{q}")
                })
                .collect();
            if updates.is_empty() {
                // All columns are part of the key — nothing to overwrite.
                // DO NOTHING semantically matches: a key match with no
                // non-key payload is already idempotent.
                format!(" ON CONFLICT ({key_quoted}) DO NOTHING")
            } else {
                format!(
                    " ON CONFLICT ({key_quoted}) DO UPDATE SET {}",
                    updates.join(", ")
                )
            }
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
    fn insert_statement_form() {
        let sql = insert_statement("analytics.events", &["event_id".into()]).unwrap();
        assert_eq!(sql, "INSERT INTO \"analytics\".\"events\" (\"event_id\") ");
    }

    #[test]
    fn conflict_suffix_ignore() {
        let cfg = ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Ignore,
        };
        let s = conflict_suffix(&cfg, &["id".into(), "name".into()]).unwrap();
        assert_eq!(s, " ON CONFLICT (\"id\") DO NOTHING");
    }

    #[test]
    fn conflict_suffix_overwrite_excludes_key_from_update() {
        let cfg = ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        };
        let s = conflict_suffix(&cfg, &["id".into(), "name".into(), "age".into()]).unwrap();
        assert_eq!(
            s,
            " ON CONFLICT (\"id\") DO UPDATE SET \"name\" = EXCLUDED.\"name\", \"age\" = EXCLUDED.\"age\""
        );
    }

    #[test]
    fn conflict_suffix_overwrite_with_key_only_columns_falls_back_to_ignore() {
        let cfg = ConflictConfig {
            key: vec!["id".into()],
            strategy: ConflictStrategy::Overwrite,
        };
        let s = conflict_suffix(&cfg, &["id".into()]).unwrap();
        assert_eq!(s, " ON CONFLICT (\"id\") DO NOTHING");
    }
}
