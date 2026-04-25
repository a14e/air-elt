//! SQL emitted by the source.
//!
//! Identifiers always go through `commons::sql::pg::identifier`; values are
//! always bound via sqlx `$N`. Statements are built once at flow setup and
//! reused across ticks (the sink builds its own INSERT per batch via
//! `QueryBuilder`).

use air_elt_commons::sql::pg::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::types::Value;

pub const PING: &str = "SELECT 1";

pub const HAS_TABLE_SELECT: &str = "SELECT has_table_privilege(current_user, $1, 'SELECT') AS ok";

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
/// NULL algebra: `NULL < any_non_null` and `NULL == NULL`. NULL is treated
/// as the minimum ("zero") element. ORDER BY uses `NULLS FIRST` for ASC
/// and `NULLS LAST` for DESC to match this algebra.
///
/// When `cursor_state` is `None` → first tick: `SELECT cols FROM t ORDER BY ...
/// LIMIT $1`. No WHERE, one bind (limit).
///
/// Path selection:
/// - ASC: null-aware only when cursor *contains* NULL. Non-null cursor uses
///   plain `(c1,c2) > ($1,$2)` — NULL rows were already read first under
///   `NULLS FIRST`, so excluding them is correct.
/// - DESC: null-aware when cursor contains NULL OR any cursor field is
///   nullable in schema. This is needed because under `NULLS LAST`, NULL
///   rows come *after* non-null cursor values, and SQL `col < val` is
///   UNKNOWN for NULL — plain compare would silently drop them.
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

    // NULL < everything → ASC NULLS FIRST, DESC NULLS LAST.
    let (order_sql, nulls_sql) = match order {
        CursorOrder::Asc => ("ASC", "NULLS FIRST"),
        CursorOrder::Desc => ("DESC", "NULLS LAST"),
    };
    let mut order_by = String::new();
    for (i, cf) in cursor_fields.iter().enumerate() {
        if i > 0 {
            order_by.push_str(", ");
        }
        let quoted = quote_ident(cf);
        order_by.push_str(&format!("{quoted} {order_sql} {nulls_sql}"));
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
        // ASC: NULLs read first (NULLS FIRST), so non-null cursor → plain compare
        //      is correct. NULL cursor → null-aware to navigate past NULL rows.
        // DESC: NULLs read last (NULLS LAST). For non-null cursor, plain compare
        //       silently drops NULL rows in nullable columns (`NULL < val` is
        //       UNKNOWN). Use null-aware whenever any cursor field is nullable
        //       in the schema, OR cursor itself contains NULL.
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
        placeholders.push_str(&format!("${}", bind_order.len()));
    }
    format!("({quoted_cols}) {cmp} ({placeholders})")
}

/// Null-aware lex-compare using `NULL < everything` algebra.
///
/// For each prefix length k: produce
///   `(prefix_eq_1..k-1) AND nullable_cmp(c_k, v_k)`
/// and OR them together.
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

/// `col IS NOT DISTINCT FROM $N` (postgres NULL-safe equality).
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
        format!("{quoted} IS NOT DISTINCT FROM ${}", bind_order.len())
    }
}

/// Strict compare against a possibly-NULL cursor value using `NULL < everything`.
/// Returns empty string when the comparison is unconditionally false.
fn nullable_cmp(
    col: &str,
    v: &Value,
    order: CursorOrder,
    bind_order: &mut Vec<usize>,
    state: &CursorState,
) -> String {
    let quoted = quote_ident(col);
    match (order, matches!(v, Value::Null)) {
        (CursorOrder::Asc, true) => {
            // ASC strict-gt: col > NULL. Since NULL is minimum, anything
            // non-null is greater. col IS NOT NULL.
            format!("{quoted} IS NOT NULL")
        }
        (CursorOrder::Desc, true) => {
            // DESC strict-lt: col < NULL. Nothing is less than the minimum.
            String::new()
        }
        (CursorOrder::Asc, false) => {
            // ASC strict-gt: col > val. Only non-null values greater than val
            // qualify. NULL < val, so NULL rows don't pass.
            let idx = state
                .fields
                .iter()
                .position(|f| f.name == col)
                .expect("cursor state validated upstream");
            bind_order.push(idx);
            format!("{quoted} > ${}", bind_order.len())
        }
        (CursorOrder::Desc, false) => {
            // DESC strict-lt: col < val. NULL < val is TRUE under our algebra,
            // so NULL rows qualify alongside non-null rows below val.
            let idx = state
                .fields
                .iter()
                .position(|f| f.name == col)
                .expect("cursor state validated upstream");
            bind_order.push(idx);
            format!("{quoted} IS NULL OR {quoted} < ${}", bind_order.len())
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
            &[false, false],
        )
        .unwrap();
        assert!(!q.sql.contains("WHERE"));
        assert!(q.sql.ends_with("LIMIT $1"));
        assert!(q.bind_order.is_empty());
        assert!(q.sql.contains("NULLS FIRST"));
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
            &[false, false],
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
            &[false],
        )
        .unwrap();
        assert!(q.sql.contains("ORDER BY \"id\" DESC NULLS LAST"));
        assert!(q.sql.contains("(\"id\") < ($1)"));
    }

    #[test]
    fn null_aware_asc_null_cursor_emits_not_null() {
        let state = state_with(vec![("id", Value::Null)]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            Some(&state),
            &[false],
        )
        .unwrap();
        // ASC + cursor NULL → NULL is minimum, anything non-null is greater.
        assert!(q.sql.contains("\"id\" IS NOT NULL"));
    }

    #[test]
    fn null_aware_desc_null_cursor_yields_false() {
        let state = state_with(vec![("id", Value::Null)]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Desc,
            Some(&state),
            &[false],
        )
        .unwrap();
        // DESC + cursor NULL → nothing is less than minimum → WHERE FALSE.
        assert!(q.sql.contains("WHERE FALSE"));
    }

    #[test]
    fn null_aware_multi_column_mixed_null() {
        let state = state_with(vec![("created_at", Value::Null), ("id", Value::Int64(42))]);
        let q = build_read_batch(
            "public.users",
            &["id".into()],
            &["created_at".into(), "id".into()],
            CursorOrder::Asc,
            Some(&state),
            &[false, false],
        )
        .unwrap();
        // k=1: created_at > NULL → created_at IS NOT NULL (any non-null is > NULL)
        // k=2: created_at IS NULL AND id > $1
        assert!(q.sql.contains("\"created_at\" IS NOT NULL"));
        assert!(q.sql.contains("\"id\" > $1"));
    }

    #[test]
    fn desc_non_null_cursor_includes_nulls() {
        let state = state_with(vec![("rank", Value::Int64(5))]);
        // rank is nullable in schema → DESC must use null-aware path.
        let q = build_read_batch(
            "public.t",
            &["id".into()],
            &["rank".into()],
            CursorOrder::Desc,
            Some(&state),
            &[true],
        )
        .unwrap();
        // DESC + nullable cursor field: col < val OR col IS NULL (NULL < val is true).
        assert!(q.sql.contains("\"rank\" IS NULL OR \"rank\" < $1"));
    }

    #[test]
    fn non_null_cursor_produces_stable_sql() {
        let s1 = state_with(vec![("id", Value::Int64(1))]);
        let s2 = state_with(vec![("id", Value::Int64(999))]);
        let q1 = build_read_batch(
            "public.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            Some(&s1),
            &[false],
        )
        .unwrap();
        let q2 = build_read_batch(
            "public.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            Some(&s2),
            &[false],
        )
        .unwrap();
        assert_eq!(q1.sql, q2.sql);
        assert_eq!(q1.bind_order, q2.bind_order);
    }

    #[test]
    fn null_cursor_produces_different_sql() {
        let non_null = state_with(vec![("id", Value::Int64(1))]);
        let with_null = state_with(vec![("id", Value::Null)]);
        let q_normal = build_read_batch(
            "public.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            Some(&non_null),
            &[false],
        )
        .unwrap();
        let q_null = build_read_batch(
            "public.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            Some(&with_null),
            &[false],
        )
        .unwrap();
        assert_ne!(q_normal.sql, q_null.sql);
    }
}
