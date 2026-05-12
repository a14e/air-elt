//! ClickHouse identifier quoting. ClickHouse uses backticks (same as
//! MySQL) for identifier quoting; `db.table` references use a dot.
//! Validation rules and `IdentifierError` come from
//! [`air_elt_commons::identifier`].

use air_elt_commons::identifier::{IdentifierError, parse_qualified, validate_segment};

const QUOTE: char = '`';
const MAX_SEGMENTS: usize = 2;

/// Backtick-quote a single identifier; double any internal backtick.
pub fn quote_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push(QUOTE);
    for ch in name.chars() {
        if ch == QUOTE {
            out.push_str("``");
        } else {
            out.push(ch);
        }
    }
    out.push(QUOTE);
    out
}

/// `db.table` → `` `db`.`table` ``. ClickHouse allows multi-tenant
/// databases on one server but no third-level catalog, so anything
/// beyond two segments is rejected. User-quoted segments
/// (`` `weird-name` ``) bypass the bare-character check.
pub fn quote_qualified(name: &str) -> Result<String, IdentifierError> {
    let segments = parse_qualified(name, QUOTE)?;
    if segments.len() > MAX_SEGMENTS {
        return Err(IdentifierError::TooManySegments {
            value: name.to_string(),
            got: segments.len(),
            max: MAX_SEGMENTS,
        });
    }
    let mut out = String::with_capacity(name.len() + segments.len() * 2);
    for (i, seg) in segments.iter().enumerate() {
        if !seg.quoted {
            validate_segment(&seg.value)?;
        } else if seg.value.is_empty() {
            return Err(IdentifierError::EmptySegment);
        }
        if i > 0 {
            out.push('.');
        }
        out.push_str(&quote_ident(&seg.value));
    }
    Ok(out)
}

/// Quote a column list comma-joined. ClickHouse Nested sub-columns
/// use dotted names (`items.label`, `items.qty`). The dot is part of
/// the column name, not a db/table separator — each name is backtick-
/// quoted as a single identifier.
pub fn quote_columns(names: &[String]) -> Result<String, IdentifierError> {
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&quote_ident(name));
    }
    Ok(out)
}

/// Split `db.table` into `(db, table)`. A bare name returns
/// `(None, table)`.
pub fn split_qualified(name: &str) -> Result<(Option<String>, String), IdentifierError> {
    let segments = parse_qualified(name, QUOTE)?;
    if segments.len() > MAX_SEGMENTS {
        return Err(IdentifierError::TooManySegments {
            value: name.to_string(),
            got: segments.len(),
            max: MAX_SEGMENTS,
        });
    }
    for seg in &segments {
        if !seg.quoted {
            validate_segment(&seg.value)?;
        } else if seg.value.is_empty() {
            return Err(IdentifierError::EmptySegment);
        }
    }
    Ok(match segments.as_slice() {
        [table] => (None, table.value.clone()),
        [db, table] => (Some(db.value.clone()), table.value.clone()),
        _ => unreachable!("len gated by MAX_SEGMENTS above"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn quotes_simple_ident() {
        assert_eq!(quote_ident("name"), "`name`");
    }

    #[test]
    fn doubles_internal_backtick() {
        assert_eq!(quote_ident("a`b"), "`a``b`");
    }

    #[test]
    fn quotes_db_table() {
        assert_eq!(quote_qualified("db.users").unwrap(), "`db`.`users`");
    }

    #[test]
    fn rejects_three_segments() {
        assert!(quote_qualified("a.b.c").is_err());
    }

    #[test]
    fn splits_qualified() {
        let (db, t) = split_qualified("db.users").unwrap();
        assert_eq!(db.as_deref(), Some("db"));
        assert_eq!(t, "users");
    }

    #[test]
    fn quote_columns_simple() {
        let cols = &["id".into(), "name".into()];
        assert_eq!(quote_columns(cols).unwrap(), "`id`, `name`");
    }

    #[test]
    fn quote_columns_dotted_nested_name() {
        // Nested sub-columns use dotted names — the dot is part of
        // the identifier, not a db/table separator.
        let cols = &["items.label".into(), "items.qty".into()];
        assert_eq!(quote_columns(cols).unwrap(), "`items.label`, `items.qty`");
    }
}
