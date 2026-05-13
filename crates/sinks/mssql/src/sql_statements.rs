//! SQL emitted by the MS SQL sink.
//!
//! Values flow as parameters (`@P1..@PN`) through `conn.query` — never
//! interpolated into the SQL text (see `project-conventions::SQL helpers`).
//!
//! Key dialect features:
//! - `MERGE ... WITH (HOLDLOCK)` for upsert (MS SQL has no `ON CONFLICT`,
//!   HOLDLOCK avoids the classic MERGE-upsert race).
//! - `DELETE ... WHERE (k1, k2) IN ((v1, v2), ...)` — works on MSSQL 2008+
//! - `WHERE 1=0` for probes.

use air_elt_commons_mssql::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::conflict::ConflictStrategy;
use air_elt_core::error::RuntimeResult;

pub const PING: &str = "SELECT 1";

pub fn probe_insert_where_false(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!(
        "INSERT INTO {quoted_table} ({cols}) SELECT {cols} FROM {quoted_table} WHERE 1=0"
    ))
}

pub fn probe_delete_where_false(table: &str) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    Ok(format!("DELETE FROM {quoted_table} WHERE 1=0"))
}

/// `INSERT INTO {tbl} ({cols}) VALUES ` — caller appends parameter tuples.
pub fn insert_prefix(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("INSERT INTO {quoted_table} ({cols}) VALUES "))
}

/// `MERGE {tbl} WITH (HOLDLOCK) AS target USING (VALUES ` — caller appends
/// parameter tuples. `WITH (HOLDLOCK)` is the documented MS SQL workaround
/// for the classic MERGE upsert race.
pub fn merge_prefix(table: &str) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    Ok(format!(
        "MERGE {quoted_table} WITH (HOLDLOCK) AS target USING (VALUES "
    ))
}

/// Build the MERGE suffix — `) AS source(...) ON ... WHEN MATCHED / WHEN NOT MATCHED`.
pub fn merge_suffix(
    columns: &[String],
    key_columns: &[String],
    strategy: ConflictStrategy,
) -> RuntimeResult<String> {
    let cols = quote_columns(columns)?;
    let source_cols: Vec<String> = columns
        .iter()
        .map(|c| format!("source.{}", quote_ident(c)))
        .collect();

    let on_clause: Vec<String> = key_columns
        .iter()
        .map(|k| {
            let qk = quote_ident(k);
            format!("target.{qk} = source.{qk}")
        })
        .collect();

    let non_key_updates: Vec<String> = columns
        .iter()
        .filter(|c| !key_columns.contains(c))
        .map(|c| {
            let qc = quote_ident(c);
            format!("{qc} = source.{qc}")
        })
        .collect();

    let when_matched = match strategy {
        ConflictStrategy::Ignore => String::new(),
        ConflictStrategy::Overwrite => {
            format!(
                " WHEN MATCHED THEN UPDATE SET {}",
                non_key_updates.join(", ")
            )
        }
    };

    Ok(format!(
        ") AS source ({cols}) ON {on}{when_matched} \
         WHEN NOT MATCHED THEN INSERT ({cols}) VALUES ({src_cols});",
        cols = cols,
        on = on_clause.join(" AND "),
        when_matched = when_matched,
        src_cols = source_cols.join(", "),
    ))
}

/// `DELETE FROM {tbl} WHERE {k} IN (` — caller appends parameter list and `)`.
pub fn delete_prefix(table: &str, key_columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    if key_columns.len() == 1 {
        let k = quote_ident(&key_columns[0]);
        Ok(format!("DELETE FROM {quoted_table} WHERE {k} IN ("))
    } else {
        let kcols: Vec<String> = key_columns.iter().map(|c| quote_ident(c)).collect();
        Ok(format!(
            "DELETE FROM {quoted_table} WHERE ({}) IN (",
            kcols.join(", ")
        ))
    }
}

/// Wrap a write statement in a single-batch try/rollback for dry-run
/// validation. The whole batch is sent as one `conn.query` so a failure on
/// the inner statement cannot leave a transaction open on the connection.
pub fn dry_run_wrap(inner: &str) -> String {
    format!(
        "BEGIN TRY \
            BEGIN TRANSACTION; \
            {inner} \
            IF @@TRANCOUNT > 0 ROLLBACK; \
         END TRY \
         BEGIN CATCH \
            IF @@TRANCOUNT > 0 ROLLBACK; \
            THROW; \
         END CATCH;"
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn probe_insert_uses_1_eq_0() {
        let sql =
            probe_insert_where_false("myschema.users", &["id".into(), "name".into()]).unwrap();
        assert!(sql.contains("WHERE 1=0"));
    }

    #[test]
    fn merge_prefix_uses_holdlock() {
        let sql = merge_prefix("myschema.users").unwrap();
        assert!(sql.contains("WITH (HOLDLOCK)"));
    }

    #[test]
    fn merge_suffix_overwrite_has_update() {
        let suffix = merge_suffix(
            &["id".into(), "name".into(), "val".into()],
            &["id".into()],
            ConflictStrategy::Overwrite,
        )
        .unwrap();
        assert!(suffix.contains("WHEN MATCHED THEN UPDATE SET"));
        assert!(suffix.contains("WHEN NOT MATCHED THEN INSERT"));
    }

    #[test]
    fn merge_suffix_ignore_no_update() {
        let suffix = merge_suffix(
            &["id".into(), "name".into()],
            &["id".into()],
            ConflictStrategy::Ignore,
        )
        .unwrap();
        assert!(!suffix.contains("WHEN MATCHED"));
        assert!(suffix.contains("WHEN NOT MATCHED THEN INSERT"));
    }

    #[test]
    fn delete_single_key() {
        let sql = delete_prefix("myschema.users", &["id".into()]).unwrap();
        assert!(sql.contains("WHERE \"id\" IN ("));
    }

    #[test]
    fn delete_compound_key() {
        let sql = delete_prefix("myschema.users", &["a".into(), "b".into()]).unwrap();
        assert!(sql.contains("WHERE (\"a\", \"b\") IN ("));
    }

    #[test]
    fn dry_run_wrap_contains_try_catch_and_rollback() {
        let s = dry_run_wrap("INSERT INTO foo VALUES (1);");
        assert!(s.contains("BEGIN TRY"));
        assert!(s.contains("BEGIN CATCH"));
        assert!(s.contains("ROLLBACK"));
        assert!(s.contains("THROW;"));
    }
}
