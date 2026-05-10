//! SQL emitted by the sink.

use air_elt_commons_pg::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::conflict::{ConflictConfig, ConflictStrategy};
use air_elt_core::error::RuntimeResult;

pub const PING: &str = "SELECT 1";

pub const HAS_TABLE_INSERT: &str = "SELECT has_table_privilege(current_user, $1, 'INSERT') AS ok";

pub const HAS_TABLE_DELETE: &str = "SELECT has_table_privilege(current_user, $1, 'DELETE') AS ok";

/// `DELETE FROM "schema"."t" WHERE false` — used by the validate-time
/// DELETE probe. Identifier-only; no values bound.
pub fn probe_delete_where_false(table: &str) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    Ok(format!("DELETE FROM {quoted_table} WHERE false"))
}

/// `INSERT INTO "schema"."t" ("c1","c2") SELECT "c1","c2" FROM "schema"."t" WHERE false`
pub fn probe_insert_where_false(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!(
        "INSERT INTO {quoted_table} ({cols}) \
         SELECT {cols} FROM {quoted_table} WHERE false"
    ))
}

/// `INSERT INTO "schema"."t" ("c1","c2") SELECT * FROM (` — caller
/// appends a `push_values` loop emitting `VALUES ($1,$2),($3,$4),...`
/// with the row payloads, then [`DRY_RUN_INSERT_SUFFIX`]. Final form is
/// `INSERT INTO "schema"."t" ("c1","c2") SELECT * FROM (VALUES ($1,$2),($3,$4)) AS sub WHERE false`.
/// This is the dry-run form: the planner parses, type-checks every
/// bind, and the `WHERE false` filter prevents any rows from reaching
/// the table.
pub fn dry_run_insert_prefix(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!(
        "INSERT INTO {quoted_table} ({cols}) SELECT * FROM ("
    ))
}

/// Closing fragment that pairs with [`dry_run_insert_prefix`].
pub const DRY_RUN_INSERT_SUFFIX: &str = ") AS sub WHERE false";

/// `DELETE FROM "schema"."t" WHERE ("k1","k2") IN (` followed by the
/// caller-pushed values then [`DRY_RUN_DELETE_SUFFIX`]. The trailing
/// `AND false` short-circuits the delete after type-checking but
/// before any row is touched.
pub const DRY_RUN_DELETE_SUFFIX: &str = ") AND false";

/// INSERT statement consumed by `QueryBuilder::push_values`; caller appends `VALUES ...`.
///
/// The same SQL is emitted for both Postgres and CockroachDB. Cockroach's
/// native `UPSERT` is *not* used because it silently uses the primary key
/// as the conflict arbiter regardless of any user-declared `conflict.key`,
/// which can mask misconfiguration. Standard `INSERT … ON CONFLICT (key)`
/// works on both engines and respects the user-declared key honestly.
pub fn insert_statement(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {quoted_table} ({cols}) "))
}

/// `DELETE FROM "schema"."t" WHERE ("k1","k2") IN (` — caller appends a
/// `push_values` loop that pushes the tuple values, then `)`.
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
