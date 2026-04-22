//! SQL emitted by the source.
//!
//! Identifiers always go through `commons::sql::pg::identifier`; values are
//! always bound via sqlx `$N`. Statements are built once at flow setup and
//! reused across ticks (the sink builds its own INSERT per batch via
//! `QueryBuilder`).

use air_elt_commons::sql::pg::identifier::{quote_columns, quote_qualified};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::error::RuntimeResult;
use air_elt_core::flow::state::CursorState;
use air_elt_core::types::Value;

pub const PING: &str = "SELECT 1";

pub const HAS_TABLE_SELECT: &str = "SELECT has_table_privilege(current_user, $1, 'SELECT') AS ok";

/// `information_schema.columns` — returns `(column_name, is_nullable,
/// udt_name, data_type)` sorted by ordinal position.
pub const INFORMATION_SCHEMA: &str = "SELECT column_name, is_nullable, udt_name, data_type
    FROM information_schema.columns
    WHERE table_schema = $1 AND table_name = $2
    ORDER BY ordinal_position";

/// Zero-row projection used by `validate_access` to check SELECT privilege
/// on the specific columns without reading any rows.
pub fn probe_select(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("SELECT {cols} FROM {quoted_table} WHERE false"))
}

pub struct ReadQuery {
    pub sql: String,
    /// Indices into `CursorState.fields` of the **non-null** positions. The
    /// caller binds values in this order; NULL positions are inlined in SQL
    /// as `IS NULL` / `IS NOT NULL` and take no bind slot. For a plain
    /// non-null cursor this is simply `0..cursor_fields.len()`.
    pub bind_order: Vec<usize>,
}

/// Build the batch-read SQL.
///
/// When `cursor_state` is `None` → first tick: `SELECT cols FROM t ORDER BY ...
/// LIMIT $1`. No WHERE, one bind (limit).
///
/// When `cursor_state` is `Some` with no NULLs → standard tuple compare:
/// `SELECT cols FROM t WHERE (c1, c2) > ($1, $2) ORDER BY ... LIMIT $3`.
///
/// When `cursor_state` is `Some` with at least one NULL → null-aware
/// lexicographic predicate. NULLs are inlined (no bind slot); non-null
/// positions bind sequentially starting at `$1`.
///
/// Why this structure: SQL three-valued NULL semantics silently drops rows
/// when `col > NULL` is UNKNOWN. The null-aware rewrite uses the engineer
/// algebra `NULL > not-null` (matching Postgres default `ORDER BY` which is
/// ASC NULLS LAST / DESC NULLS FIRST).
pub fn build_read_batch(
    table: &str,
    columns: &[String],
    cursor_fields: &[String],
    order: CursorOrder,
    cursor_state: Option<&CursorState>,
) -> RuntimeResult<ReadQuery> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;

    // ORDER BY "c1" ASC, "c2" ASC — per-column direction so DESC applies to
    // every cursor field, not just the last one (SQL-standard foot-gun).
    let order_sql = match order {
        CursorOrder::Asc => "ASC",
        CursorOrder::Desc => "DESC",
    };
    let mut order_by = String::new();
    for (i, cf) in cursor_fields.iter().enumerate() {
        if i > 0 {
            order_by.push_str(", ");
        }
        let quoted = air_elt_commons::sql::pg::identifier::quote_ident(cf);
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
                    .expect("cursor state validated against cursor_fields upstream");
                (name.as_str(), &f.value)
            })
            .collect();

        let has_null = fields.iter().any(|(_, v)| matches!(v, Value::Null));
        if has_null {
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
    sql.push_str(&format!(" LIMIT ${}", bind_order.len() + 1));

    Ok(ReadQuery { sql, bind_order })
}

/// `(c1, c2) > ($1, $2)` / `(c1, c2) < ($1, $2)` fast path.
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
        .map(|c| air_elt_commons::sql::pg::identifier::quote_ident(c))
        .collect::<Vec<_>>()
        .join(", ");
    let mut placeholders = String::new();
    for (pos, name) in cursor_fields.iter().enumerate() {
        if pos > 0 {
            placeholders.push_str(", ");
        }
        // map the cursor-field position to its index in state.fields
        let idx = state
            .fields
            .iter()
            .position(|f| f.name == *name)
            .expect("cursor state validated upstream");
        bind_order.push(idx);
        placeholders.push_str(&format!("${}", bind_order.len()));
    }
    format!("({quoted_cols}) {cmp} ({placeholders})")
}

/// Null-aware lex-compare.
///
/// For each prefix length k: produce
///   `(prefix_eq_1..k-1) AND nullable_cmp(c_k, v_k)`
/// and OR them together.
///
/// `nullable_gt(col, Value) =
///     (col IS NULL AND v_is_not_null) OR
///     (col IS NOT NULL AND v_is_not_null AND col > $N)
///  // if v is NULL: (col IS NULL AND FALSE) OR (col IS NOT NULL AND NULL > col?)
///  // we skip emitting col > NULL because it is always false under our
///  // algebra, and the OR-chain stops early via `nullable_eq`. Concretely:
///  if v is NULL, nullable_gt becomes FALSE (no row is strictly greater than NULL)
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
        // If the strict-compare at position k-1 is unconditionally false
        // (e.g. v is NULL with ASC and NULL > X never holds), drop that OR-branch.
        if cmp_clause.is_empty() {
            continue;
        }
        parts.push(cmp_clause);
        clauses.push(format!("({})", parts.join(" AND ")));
    }
    if clauses.is_empty() {
        // All cursor positions are NULL and the algebra yields nothing strictly
        // greater (ASC) / strictly less (DESC). Emit a `FALSE` marker so the
        // batch returns 0 rows — the runner will then idle/exit cleanly.
        return "FALSE".to_string();
    }
    clauses.join(" OR ")
}

/// `col IS NOT DISTINCT FROM $N` (postgres NULL-safe equality).
fn nullable_eq(col: &str, v: &Value, bind_order: &mut Vec<usize>, state: &CursorState) -> String {
    let quoted = air_elt_commons::sql::pg::identifier::quote_ident(col);
    if matches!(v, Value::Null) {
        format!("{quoted} IS NULL")
    } else {
        let idx = state
            .fields
            .iter()
            .position(|f| f.name == col)
            .expect("cursor state validated upstream");
        bind_order.push(idx);
        format!("{quoted} IS NOT DISTINCT FROM ${}", bind_order.len())
    }
}

/// Strict compare against a possibly-NULL cursor value. Returns empty string
/// when the comparison is unconditionally false under our algebra so the
/// caller can drop that OR-branch.
fn nullable_cmp(
    col: &str,
    v: &Value,
    order: CursorOrder,
    bind_order: &mut Vec<usize>,
    state: &CursorState,
) -> String {
    let quoted = air_elt_commons::sql::pg::identifier::quote_ident(col);
    match (order, matches!(v, Value::Null)) {
        (CursorOrder::Asc, true) => {
            // NULL > X is true only for col=NULL, X=not-null — but v IS NULL
            // here. With `nulls last` semantics `NULL > NULL` = false; therefore
            // no row with col=X is strictly greater than a NULL cursor. Empty.
            String::new()
        }
        (CursorOrder::Desc, true) => {
            // DESC strict-less: col < NULL → col IS NOT NULL.
            format!("{quoted} IS NOT NULL")
        }
        (CursorOrder::Asc, false) => {
            let idx = state
                .fields
                .iter()
                .position(|f| f.name == col)
                .expect("cursor state validated upstream");
            bind_order.push(idx);
            format!("({quoted} IS NULL) OR ({quoted} > ${})", bind_order.len())
        }
        (CursorOrder::Desc, false) => {
            // DESC strict-less under nulls-last algebra: col < v iff
            // (col IS NOT NULL AND col < v). NULL is "greater" so NULL < v is false.
            let idx = state
                .fields
                .iter()
                .position(|f| f.name == col)
                .expect("cursor state validated upstream");
            bind_order.push(idx);
            format!("{quoted} IS NOT NULL AND {quoted} < ${}", bind_order.len())
        }
    }
}

/// Split `schema.table` → `(schema, table)`. A bare name falls back to `public`.
pub use air_elt_commons::sql::pg::identifier::split_qualified;

#[cfg(test)]
mod tests {
    use super::*;
    use air_elt_core::flow::state::CursorFieldValue;

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
        let sql = probe_select("public.users", &["id".into(), "name".into()]).unwrap();
        assert_eq!(
            sql,
            "SELECT \"id\", \"name\" FROM \"public\".\"users\" WHERE false"
        );
    }

    #[test]
    fn read_batch_first_tick_has_no_where_and_limit_is_dollar_1() {
        let q = build_read_batch(
            "public.users",
            &["id".into(), "created_at".into()],
            &["created_at".into(), "id".into()],
            CursorOrder::Asc,
            None,
        )
        .unwrap();
        assert!(!q.sql.contains("WHERE"));
        assert!(q.sql.ends_with("LIMIT $1"));
        assert!(q.bind_order.is_empty());
    }

    #[test]
    fn read_batch_plain_tuple_for_non_null_cursor() {
        let state = state_with(vec![
            ("created_at", Value::Int64(100)),
            ("id", Value::Int64(42)),
        ]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["created_at".into(), "id".into()],
            CursorOrder::Asc,
            Some(&state),
        )
        .unwrap();
        assert!(q.sql.contains("WHERE (\"created_at\", \"id\") > ($1, $2)"));
        assert!(q.sql.ends_with("LIMIT $3"));
        assert_eq!(q.bind_order, vec![0, 1]);
    }

    #[test]
    fn read_batch_per_column_desc() {
        let state = state_with(vec![("id", Value::Int64(42))]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Desc,
            Some(&state),
        )
        .unwrap();
        assert!(q.sql.contains("ORDER BY \"id\" DESC"));
        assert!(q.sql.contains("(\"id\") < ($1)"));
    }

    #[test]
    fn null_aware_asc_null_cursor_yields_false() {
        let state = state_with(vec![("id", Value::Null)]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            Some(&state),
        )
        .unwrap();
        // ASC + cursor NULL → nothing is strictly greater than NULL under
        // nulls-last semantics. WHERE FALSE means zero rows, drain completes.
        assert!(q.sql.contains("WHERE FALSE"));
    }

    #[test]
    fn null_aware_desc_null_cursor_emits_not_null() {
        let state = state_with(vec![("id", Value::Null)]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Desc,
            Some(&state),
        )
        .unwrap();
        assert!(q.sql.contains("\"id\" IS NOT NULL"));
    }
}
