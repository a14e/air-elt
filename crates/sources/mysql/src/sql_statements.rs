//! SQL emitted by the MySQL source.
//!
//! Identifiers go through `air_elt_commons_mysql::identifier`; values are
//! bound via sqlx `?` placeholders.
//!
//! MySQL specifics vs the pg implementation:
//! - `<=>` (NULL-safe equal) instead of `IS NOT DISTINCT FROM`.
//! - Bind placeholders are `?`, not `$N` — no positional numbering needed.
//! - `ORDER BY col ASC` is already NULLS-FIRST in MySQL (NULL sorts as the
//!   minimum), and `ORDER BY col DESC` is already NULLS-LAST. So no
//!   explicit `NULLS FIRST/LAST` is emitted — the implicit ordering matches
//!   the project's "NULL is minimum" algebra.

use air_elt_commons_mysql::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::types::Value;

pub const PING: &str = "SELECT 1";

pub fn probe_select(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("SELECT {cols} FROM {quoted_table} WHERE FALSE"))
}

pub struct ReadQuery {
    pub sql: String,
    /// See pg counterpart — same semantics. Indices into `CursorState.fields`
    /// of the **non-null** positions, in bind order.
    pub bind_order: Vec<usize>,
}

/// Build the batch-read SQL. See pg counterpart for the algorithm — only
/// dialect-specific bits differ.
pub fn build_read_batch(
    table: &str,
    columns: &[String],
    cursor_fields: &[String],
    order: CursorOrder,
    cursor_state: Option<&CursorState>,
    cursor_nullable: &[bool],
) -> RuntimeResult<ReadQuery> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;

    let order_sql = match order {
        CursorOrder::Asc => "ASC",
        CursorOrder::Desc => "DESC",
    };
    let mut order_by = String::new();
    for (i, cf) in cursor_fields.iter().enumerate() {
        if i > 0 {
            order_by.push_str(", ");
        }
        let quoted = quote_ident(cf);
        order_by.push_str(&format!("{quoted} {order_sql}"));
    }

    let mut sql = format!("SELECT {cols} FROM {quoted_table}");
    let mut bind_order = Vec::new();

    if let Some(state) = cursor_state {
        let fields: Vec<(&str, &Value)> = cursor_fields
            .iter()
            .map(|name| {
                let f = state
                    .fields
                    .iter()
                    .find(|f| f.name == *name)
                    .ok_or_else(|| {
                        RuntimeError::Other(format!(
                            "cursor field {name:?} not found in persisted cursor state"
                        ))
                    })?;
                Ok((name.as_str(), &f.value))
            })
            .collect::<RuntimeResult<Vec<_>>>()?;

        let has_null = fields.iter().any(|(_, v)| matches!(v, Value::Null));
        let needs_null_aware = match order {
            CursorOrder::Asc => has_null,
            CursorOrder::Desc => has_null || cursor_nullable.iter().any(|n| *n),
        };
        if needs_null_aware {
            sql.push_str(" WHERE ");
            sql.push_str(&null_aware_gt(&fields, order, &mut bind_order, state));
        } else {
            sql.push_str(" WHERE ");
            sql.push_str(&plain_tuple_compare(
                cursor_fields,
                order,
                &mut bind_order,
                state,
            ));
        }
    }

    sql.push_str(&format!(" ORDER BY {order_by}"));
    sql.push_str(" LIMIT ?");

    Ok(ReadQuery { sql, bind_order })
}

/// `(c1, c2) > (?, ?)` / `(c1, c2) < (?, ?)`. MySQL supports row constructors
/// for comparison just like pg.
fn plain_tuple_compare(
    cursor_fields: &[String],
    order: CursorOrder,
    bind_order: &mut Vec<usize>,
    state: &CursorState,
) -> String {
    let cmp = if matches!(order, CursorOrder::Asc) {
        ">"
    } else {
        "<"
    };
    let quoted_cols = cursor_fields
        .iter()
        .map(|c| quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut placeholders = String::new();
    for (pos, name) in cursor_fields.iter().enumerate() {
        if pos > 0 {
            placeholders.push_str(", ");
        }
        let idx = state
            .fields
            .iter()
            .position(|f| f.name == *name)
            .expect("cursor state validated upstream");
        bind_order.push(idx);
        placeholders.push('?');
    }
    format!("({quoted_cols}) {cmp} ({placeholders})")
}

/// Null-aware lex-compare. Same algorithm as pg's, with `<=>` instead of
/// `IS NOT DISTINCT FROM`.
fn null_aware_gt(
    fields: &[(&str, &Value)],
    order: CursorOrder,
    bind_order: &mut Vec<usize>,
    state: &CursorState,
) -> String {
    let mut clauses: Vec<String> = Vec::new();
    for k in 1..=fields.len() {
        let mut parts: Vec<String> = Vec::new();
        for (name, v) in &fields[..k - 1] {
            parts.push(nullable_eq(name, v, bind_order, state));
        }
        let (name, v) = fields[k - 1];
        let cmp_clause = nullable_cmp(name, v, order, bind_order, state);
        if cmp_clause.is_empty() {
            continue;
        }
        parts.push(cmp_clause);
        clauses.push(format!("({})", parts.join(" AND ")));
    }
    if clauses.is_empty() {
        return "FALSE".to_string();
    }
    clauses.join(" OR ")
}

fn nullable_eq(col: &str, v: &Value, bind_order: &mut Vec<usize>, state: &CursorState) -> String {
    let quoted = quote_ident(col);
    if matches!(v, Value::Null) {
        format!("{quoted} IS NULL")
    } else {
        let idx = state
            .fields
            .iter()
            .position(|f| f.name == col)
            .expect("cursor state validated upstream");
        bind_order.push(idx);
        format!("{quoted} <=> ?")
    }
}

fn nullable_cmp(
    col: &str,
    v: &Value,
    order: CursorOrder,
    bind_order: &mut Vec<usize>,
    state: &CursorState,
) -> String {
    let quoted = quote_ident(col);
    match (order, matches!(v, Value::Null)) {
        (CursorOrder::Asc, true) => format!("{quoted} IS NOT NULL"),
        (CursorOrder::Desc, true) => String::new(),
        (CursorOrder::Asc, false) => {
            let idx = state
                .fields
                .iter()
                .position(|f| f.name == col)
                .expect("cursor state validated upstream");
            bind_order.push(idx);
            format!("{quoted} > ?")
        }
        (CursorOrder::Desc, false) => {
            let idx = state
                .fields
                .iter()
                .position(|f| f.name == col)
                .expect("cursor state validated upstream");
            bind_order.push(idx);
            format!("{quoted} IS NULL OR {quoted} < ?")
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use air_elt_core::model::CursorFieldValue;

    fn state_with(values: Vec<(&str, Value)>) -> CursorState {
        CursorState::new(
            values
                .into_iter()
                .map(|(name, value)| CursorFieldValue {
                    name: name.to_string(),
                    value,
                })
                .collect(),
        )
    }

    #[test]
    fn probe_select_form() {
        let sql = probe_select("appdb.users", &["id".into(), "name".into()]).unwrap();
        assert_eq!(sql, "SELECT `id`, `name` FROM `appdb`.`users` WHERE FALSE");
    }

    #[test]
    fn first_tick_no_where() {
        let q = build_read_batch(
            "appdb.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            None,
            &[false],
        )
        .unwrap();
        assert!(!q.sql.contains("WHERE"));
        assert!(q.sql.ends_with("LIMIT ?"));
        assert!(q.bind_order.is_empty());
    }

    #[test]
    fn plain_tuple_for_non_null_cursor() {
        let state = state_with(vec![
            ("created_at", Value::Int64(100)),
            ("id", Value::Int64(42)),
        ]);
        let q = build_read_batch(
            "appdb.users",
            &["id".into()],
            &["created_at".into(), "id".into()],
            CursorOrder::Asc,
            Some(&state),
            &[false, false],
        )
        .unwrap();
        assert!(q.sql.contains("WHERE (`created_at`, `id`) > (?, ?)"));
        assert!(q.sql.ends_with("LIMIT ?"));
        assert_eq!(q.bind_order, vec![0, 1]);
    }

    #[test]
    fn null_cursor_uses_is_null_not_distinct_from_emulation() {
        // Null-aware path uses `<=>` for equality.
        let state = state_with(vec![("a", Value::Int64(1)), ("b", Value::Null)]);
        let q = build_read_batch(
            "appdb.t",
            &["a".into(), "b".into()],
            &["a".into(), "b".into()],
            CursorOrder::Asc,
            Some(&state),
            &[false, true],
        )
        .unwrap();
        assert!(q.sql.contains("`a` <=> ?"));
    }

    #[test]
    fn desc_null_cursor_yields_false() {
        let state = state_with(vec![("id", Value::Null)]);
        let q = build_read_batch(
            "appdb.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Desc,
            Some(&state),
            &[false],
        )
        .unwrap();
        assert!(q.sql.contains("WHERE FALSE"));
    }

    #[test]
    fn order_by_no_explicit_nulls_clause() {
        let q = build_read_batch(
            "appdb.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            None,
            &[true],
        )
        .unwrap();
        assert!(!q.sql.contains("NULLS"));
        assert!(q.sql.contains("ORDER BY `id` ASC"));
    }
}
