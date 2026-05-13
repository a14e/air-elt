//! SQL emitted by the MS SQL source.
//!
//! All cursor values flow as parameters (`@P1..@PN`) — never interpolated
//! into the SQL text. The output of `build_read_batch` is a tuple of
//! `(sql, Vec<&Value, DataType>)` so the call site can bind via
//! `air_elt_commons_mssql::value_bind::value_to_column_data`.
//!
//! MS SQL specifics vs pg/mysql:
//! - `OFFSET 0 ROWS FETCH NEXT N ROWS ONLY` instead of `LIMIT N`.
//! - No `IS NOT DISTINCT FROM` or `<=>` — non-null equality uses `=`,
//!   null equality uses `IS NULL`.
//! - `WHERE 1=0` instead of `WHERE FALSE`.
//! - Row constructors `(c1, c2) > (v1, v2)` (MSSQL 2008+).

use air_elt_commons_mssql::identifier::{quote_columns, quote_ident, quote_qualified};
use air_elt_core::config::model::CursorOrder;
use air_elt_core::error::{RuntimeError, RuntimeResult};
use air_elt_core::model::CursorState;
use air_elt_core::types::{DataType, Value};

pub const PING: &str = "SELECT 1";

pub fn probe_select(table: &str, columns: &[String]) -> RuntimeResult<String> {
    let quoted_table = quote_qualified(table)?;
    let cols = quote_columns(columns)?;
    Ok(format!("SELECT {cols} FROM {quoted_table} WHERE 1=0"))
}

/// Output of `build_read_batch`: a SQL string with `@P1..@PN` placeholders
/// and a parallel list of cursor values to bind (with their declared type
/// for typed-NULL disambiguation).
pub struct ReadQuery {
    pub sql: String,
    /// Cloned `Value`s in placeholder order. Empty for first-tick reads.
    pub params: Vec<Value>,
    /// `DataType` for each parameter (matches `params` 1:1).
    pub param_types: Vec<DataType>,
}

/// Build the batch-read SQL. Cursor values are bound as `@P1..@PN`.
#[allow(clippy::too_many_arguments)]
pub fn build_read_batch(
    table: &str,
    columns: &[String],
    cursor_fields: &[String],
    order: CursorOrder,
    cursor_state: Option<&CursorState>,
    cursor_nullable: &[bool],
    cursor_types: &[DataType],
    batch_limit: usize,
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
    let mut params: Vec<Value> = Vec::new();
    let mut param_types: Vec<DataType> = Vec::new();

    if let Some(state) = cursor_state {
        let fields: Vec<(&str, &Value, &DataType)> = cursor_fields
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let f = state
                    .fields
                    .iter()
                    .find(|f| f.name == *name)
                    .ok_or_else(|| {
                        RuntimeError::Other(format!(
                            "cursor field {name:?} not found in persisted cursor state"
                        ))
                    })?;
                Ok((name.as_str(), &f.value, &cursor_types[idx]))
            })
            .collect::<RuntimeResult<Vec<_>>>()?;

        let has_null = fields.iter().any(|(_, v, _)| matches!(v, Value::Null));
        let needs_null_aware = match order {
            CursorOrder::Asc => has_null,
            CursorOrder::Desc => has_null || cursor_nullable.iter().any(|n| *n),
        };
        if needs_null_aware {
            sql.push_str(" WHERE ");
            let predicate = null_aware_gt(&fields, order, &mut params, &mut param_types);
            sql.push_str(&predicate);
        } else {
            sql.push_str(" WHERE ");
            let predicate =
                plain_tuple_compare(cursor_fields, order, &fields, &mut params, &mut param_types);
            sql.push_str(&predicate);
        }
    }

    sql.push_str(&format!(" ORDER BY {order_by}"));
    sql.push_str(&format!(
        " OFFSET 0 ROWS FETCH NEXT {batch_limit} ROWS ONLY"
    ));

    Ok(ReadQuery {
        sql,
        params,
        param_types,
    })
}

fn placeholder(params_len: usize) -> String {
    format!("@P{}", params_len + 1)
}

/// `(c1, c2) > (@Pn, @Pn+1)`.
fn plain_tuple_compare(
    cursor_fields: &[String],
    order: CursorOrder,
    values: &[(&str, &Value, &DataType)],
    params: &mut Vec<Value>,
    param_types: &mut Vec<DataType>,
) -> String {
    let cmp = if matches!(order, CursorOrder::Asc) {
        ">"
    } else {
        "<"
    };
    let quoted_cols: Vec<String> = cursor_fields.iter().map(|c| quote_ident(c)).collect();
    let placeholders: Vec<String> = values
        .iter()
        .map(|(_, v, t)| {
            let ph = placeholder(params.len());
            params.push((*v).clone());
            param_types.push((*t).clone());
            ph
        })
        .collect();
    format!(
        "({}) {cmp} ({})",
        quoted_cols.join(", "),
        placeholders.join(", ")
    )
}

/// Null-aware lex-compare with parameter bindings.
fn null_aware_gt(
    fields: &[(&str, &Value, &DataType)],
    order: CursorOrder,
    params: &mut Vec<Value>,
    param_types: &mut Vec<DataType>,
) -> String {
    let mut clauses: Vec<String> = Vec::new();
    for k in 1..=fields.len() {
        let mut parts: Vec<String> = Vec::new();
        for (name, v, t) in &fields[..k - 1] {
            parts.push(nullable_eq(name, v, t, params, param_types));
        }
        let (name, v, t) = fields[k - 1];
        let cmp_clause = nullable_cmp(name, v, t, order, params, param_types);
        if cmp_clause.is_empty() {
            continue;
        }
        parts.push(cmp_clause);
        clauses.push(format!("({})", parts.join(" AND ")));
    }
    if clauses.is_empty() {
        "1=0".to_string()
    } else {
        clauses.join(" OR ")
    }
}

fn nullable_eq(
    col: &str,
    v: &Value,
    t: &DataType,
    params: &mut Vec<Value>,
    param_types: &mut Vec<DataType>,
) -> String {
    let quoted = quote_ident(col);
    if matches!(v, Value::Null) {
        format!("{quoted} IS NULL")
    } else {
        let ph = placeholder(params.len());
        params.push(v.clone());
        param_types.push(t.clone());
        format!("{quoted} = {ph}")
    }
}

fn nullable_cmp(
    col: &str,
    v: &Value,
    t: &DataType,
    order: CursorOrder,
    params: &mut Vec<Value>,
    param_types: &mut Vec<DataType>,
) -> String {
    let quoted = quote_ident(col);
    match (order, matches!(v, Value::Null)) {
        (CursorOrder::Asc, true) => format!("{quoted} IS NOT NULL"),
        (CursorOrder::Desc, true) => String::new(),
        (CursorOrder::Asc, false) => {
            let ph = placeholder(params.len());
            params.push(v.clone());
            param_types.push(t.clone());
            format!("{quoted} > {ph}")
        }
        (CursorOrder::Desc, false) => {
            let ph = placeholder(params.len());
            params.push(v.clone());
            param_types.push(t.clone());
            format!("{quoted} IS NULL OR {quoted} < {ph}")
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
    fn probe_select_uses_1_eq_0() {
        let sql = probe_select("myschema.users", &["id".into(), "name".into()]).unwrap();
        assert_eq!(
            sql,
            "SELECT \"id\", \"name\" FROM \"myschema\".\"users\" WHERE 1=0"
        );
    }

    #[test]
    fn first_tick_no_where_no_params() {
        let q = build_read_batch(
            "myschema.users",
            &["id".into()],
            &["id".into()],
            CursorOrder::Asc,
            None,
            &[false],
            &[DataType::Int64],
            100,
        )
        .unwrap();
        assert!(!q.sql.contains("WHERE"));
        assert!(q.sql.contains("OFFSET 0 ROWS FETCH NEXT 100 ROWS ONLY"));
        assert!(q.params.is_empty());
    }

    #[test]
    fn plain_tuple_for_non_null_cursor() {
        let state = state_with(vec![
            ("created_at", Value::Int64(100)),
            ("id", Value::Int64(42)),
        ]);
        let q = build_read_batch(
            "myschema.users",
            &["id".into()],
            &["created_at".into(), "id".into()],
            CursorOrder::Asc,
            Some(&state),
            &[false, false],
            &[DataType::Int64, DataType::Int64],
            50,
        )
        .unwrap();
        assert!(q.sql.contains("(\"created_at\", \"id\") > (@P1, @P2)"));
        assert_eq!(q.params.len(), 2);
        assert_eq!(q.params[0], Value::Int64(100));
        assert_eq!(q.params[1], Value::Int64(42));
    }

    #[test]
    fn null_cursor_asc_uses_is_not_null() {
        let state = state_with(vec![("a", Value::Int64(1)), ("b", Value::Null)]);
        let q = build_read_batch(
            "myschema.t",
            &["a".into(), "b".into()],
            &["a".into(), "b".into()],
            CursorOrder::Asc,
            Some(&state),
            &[false, true],
            &[DataType::Int64, DataType::Int64],
            10,
        )
        .unwrap();
        assert!(q.sql.contains("\"a\" = @P"));
        assert!(q.sql.contains("IS NOT NULL"));
    }

    #[test]
    fn desc_null_cursor_yields_1_eq_0() {
        let state = state_with(vec![("id", Value::Null)]);
        let q = build_read_batch(
            "myschema.t",
            &["id".into()],
            &["id".into()],
            CursorOrder::Desc,
            Some(&state),
            &[false],
            &[DataType::Int64],
            10,
        )
        .unwrap();
        assert!(q.sql.contains("1=0"));
    }
}
