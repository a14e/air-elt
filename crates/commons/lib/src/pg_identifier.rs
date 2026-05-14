//! Shared SQL-92 standard double-quote identifier quoting.
//!
//! Postgres, QuestDB, and any other engine that follows the SQL-92
//! identifier-quoting convention use the same rules: wrap with `"…"`,
//! escape an internal `"` by doubling it. The validation rules and
//! `IdentifierError` come from [`crate::identifier`]; only the dialect
//! re-exports differ between callers (pg adds `split_qualified` returning
//! a `public.` default schema; QuestDB does not).

use crate::identifier::{IdentifierError, parse_qualified, validate_segment};

const QUOTE: char = '"';
const MAX_SEGMENTS: usize = 2;

/// Double-quote an identifier; double any internal `"`. Infallible — the
/// caller is responsible for any character-class checks ahead of time
/// (`quote_qualified` and `quote_columns` run [`validate_segment`] for
/// bare segments).
pub fn quote_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push(QUOTE);
    for ch in name.chars() {
        if ch == QUOTE {
            out.push_str("\"\"");
        } else {
            out.push(ch);
        }
    }
    out.push(QUOTE);
    out
}

/// `schema.table` → `"schema"."table"`. Accepts up to two dotted
/// segments. Each bare segment is validated; a user-provided quoted
/// segment (e.g. `"My.Schema"."tbl"`) bypasses the bare-character check
/// — the quotes are an explicit opt-in to raw text.
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
/// already wrapped in `"..."` to opt out of bare-character validation.
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

/// Split a dotted identifier into `(schema, table)`. A bare name falls back
/// to `public` so callers can pass the result straight to
/// `information_schema`. Validates segment count and characters identically
/// to [`quote_qualified`]. Used by the pg dialect; QuestDB has no schema
/// tier so it does not call this helper.
pub fn split_qualified(name: &str) -> Result<(String, String), IdentifierError> {
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
        [table] => ("public".to_string(), table.value.clone()),
        [schema, table] => (schema.value.clone(), table.value.clone()),
        _ => unreachable!("segments.len() bounded by check above"),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_wraps_in_double_quotes() {
        assert_eq!(quote_ident("users"), "\"users\"");
    }

    #[test]
    fn quote_ident_doubles_internal_quote() {
        assert_eq!(quote_ident("he\"llo"), "\"he\"\"llo\"");
    }

    #[test]
    fn quote_ident_allows_underscore_leading_name() {
        assert_eq!(quote_ident("_value"), "\"_value\"");
    }

    #[test]
    fn quote_qualified_emits_dot_outside_quotes() {
        assert_eq!(
            quote_qualified("schema.table").unwrap(),
            "\"schema\".\"table\""
        );
    }

    #[test]
    fn quote_qualified_single_segment() {
        assert_eq!(quote_qualified("users").unwrap(), "\"users\"");
    }

    #[test]
    fn quote_qualified_accepts_user_quoted_special_chars() {
        // dashes / dots / spaces inside a quoted segment are user-opt-in.
        assert_eq!(
            quote_qualified("\"My.Schema\".\"weird-name\"").unwrap(),
            "\"My.Schema\".\"weird-name\""
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
    fn quote_qualified_rejects_empty_segment() {
        assert!(quote_qualified(".table").is_err());
        assert!(quote_qualified("schema.").is_err());
        assert!(quote_qualified("\"\".table").is_err());
    }

    #[test]
    fn quote_qualified_rejects_unterminated_quote() {
        assert!(matches!(
            quote_qualified("\"oops"),
            Err(IdentifierError::UnterminatedQuote { .. })
        ));
    }

    #[test]
    fn quote_qualified_db_table() {
        assert_eq!(quote_qualified("db.metrics").unwrap(), "\"db\".\"metrics\"");
    }

    #[test]
    fn quote_columns_comma_joined() {
        let cols = vec!["id".into(), "name".into()];
        assert_eq!(quote_columns(&cols).unwrap(), "\"id\", \"name\"");
    }

    #[test]
    fn quote_columns_accepts_quoted_with_special_chars() {
        let cols = vec!["\"weird-name\"".into(), "id".into()];
        assert_eq!(quote_columns(&cols).unwrap(), "\"weird-name\", \"id\"");
    }

    #[test]
    fn quote_columns_rejects_bad_name() {
        let cols = &["ok".into(), "bad name".into()];
        assert!(quote_columns(cols).is_err());
    }

    #[test]
    fn split_qualified_defaults_public() {
        assert_eq!(
            split_qualified("users").unwrap(),
            ("public".into(), "users".into())
        );
        assert_eq!(
            split_qualified("schema.users").unwrap(),
            ("schema".into(), "users".into())
        );
    }

    #[test]
    fn split_qualified_unwraps_quoted_segments() {
        assert_eq!(
            split_qualified("\"My.Schema\".\"My Table\"").unwrap(),
            ("My.Schema".into(), "My Table".into())
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
