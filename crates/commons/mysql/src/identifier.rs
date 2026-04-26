//! MySQL identifier quoting. Backticks instead of pg's double quotes.
//! Validation rules and `IdentifierError` come from `air_elt_commons::identifier`.

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

/// `db.table` → `` `db`.`table` ``. MySQL has no third-level catalog, so
/// anything beyond two segments is rejected. User-quoted segments
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

/// Quote a column list comma-joined. Individual columns may be passed
/// already wrapped in backticks to opt out of bare-character validation.
pub fn quote_columns(names: &[String]) -> Result<String, IdentifierError> {
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        let segments = parse_qualified(name, QUOTE)?;
        if segments.len() != 1 {
            return Err(IdentifierError::TooManySegments {
                value: name.clone(),
                got: segments.len(),
                max: 1,
            });
        }
        let seg = &segments[0];
        if !seg.quoted {
            validate_segment(&seg.value)?;
        } else if seg.value.is_empty() {
            return Err(IdentifierError::EmptySegment);
        }
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&quote_ident(&seg.value));
    }
    Ok(out)
}

/// Split a `db.table` into `(db, table)`. A bare name returns `(None, table)`
/// so the caller can fall back to `SELECT DATABASE()`. Validates segment
/// count and characters identically to `quote_qualified`.
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
        _ => unreachable!("segments.len() bounded by check above"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_wraps_in_backticks() {
        assert_eq!(quote_ident("users"), "`users`");
    }

    #[test]
    fn quote_ident_doubles_internal_backtick() {
        assert_eq!(quote_ident("he`llo"), "`he``llo`");
    }

    #[test]
    fn quote_qualified_two_segments() {
        assert_eq!(quote_qualified("appdb.users").unwrap(), "`appdb`.`users`");
    }

    #[test]
    fn quote_qualified_accepts_user_quoted_special_chars() {
        assert_eq!(
            quote_qualified("`my-db`.`weird name`").unwrap(),
            "`my-db`.`weird name`"
        );
    }

    #[test]
    fn quote_qualified_rejects_three_segments() {
        assert!(matches!(
            quote_qualified("a.b.c"),
            Err(IdentifierError::TooManySegments { .. })
        ));
    }

    #[test]
    fn quote_qualified_rejects_special_chars_unquoted() {
        assert!(quote_qualified("foo bar").is_err());
        assert!(quote_qualified("foo-bar").is_err());
    }

    #[test]
    fn quote_qualified_rejects_unterminated_quote() {
        assert!(matches!(
            quote_qualified("`oops"),
            Err(IdentifierError::UnterminatedQuote { .. })
        ));
    }

    #[test]
    fn quote_columns_comma_joined() {
        let cols = vec!["id".into(), "name".into()];
        assert_eq!(quote_columns(&cols).unwrap(), "`id`, `name`");
    }

    #[test]
    fn quote_columns_accepts_quoted_with_special_chars() {
        let cols = vec!["`weird-name`".into(), "id".into()];
        assert_eq!(quote_columns(&cols).unwrap(), "`weird-name`, `id`");
    }

    #[test]
    fn split_qualified_handles_bare_name() {
        assert_eq!(split_qualified("users").unwrap(), (None, "users".into()));
        assert_eq!(
            split_qualified("appdb.users").unwrap(),
            (Some("appdb".into()), "users".into())
        );
    }

    #[test]
    fn split_qualified_unwraps_quoted_segments() {
        assert_eq!(
            split_qualified("`my-db`.`My Table`").unwrap(),
            (Some("my-db".into()), "My Table".into())
        );
    }

    #[test]
    fn split_qualified_rejects_three_segments() {
        assert!(matches!(
            split_qualified("a.b.c"),
            Err(IdentifierError::TooManySegments { .. })
        ));
    }

    #[test]
    fn split_qualified_rejects_bad_chars() {
        assert!(split_qualified("foo bar").is_err());
    }
}
