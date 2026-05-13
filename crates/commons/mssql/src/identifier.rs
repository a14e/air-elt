//! MS SQL identifier quoting. Double quotes + SET QUOTED_IDENTIFIER ON.
//! Validation rules and `IdentifierError` come from `air_elt_commons::identifier`.

use air_elt_commons::identifier::{IdentifierError, parse_qualified, validate_segment};

const QUOTE: char = '"';
const MAX_SEGMENTS: usize = 2;
const DEFAULT_SCHEMA: &str = "dbo";

/// Double-quote a single identifier; double any internal double-quote.
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

/// `schema.table` → `"schema"."table"`. Bare table names default to `"dbo"`.
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
    let segs: Vec<_> = if segments.len() == 1 {
        vec![
            air_elt_commons::identifier::ParsedSegment {
                value: DEFAULT_SCHEMA.to_string(),
                quoted: false,
            },
            segments[0].clone(),
        ]
    } else {
        segments
    };
    for (i, seg) in segs.iter().enumerate() {
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

/// Quote a column list comma-joined.
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

/// Split a `schema.table` into `(schema, table)`. A bare name returns
/// `("dbo", table)`.
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
        [table] => (DEFAULT_SCHEMA.to_string(), table.value.clone()),
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
    fn quote_qualified_two_segments() {
        assert_eq!(
            quote_qualified("myschema.users").unwrap(),
            "\"myschema\".\"users\""
        );
    }

    #[test]
    fn quote_qualified_bare_table_defaults_to_dbo() {
        assert_eq!(quote_qualified("users").unwrap(), "\"dbo\".\"users\"");
    }

    #[test]
    fn quote_qualified_accepts_user_quoted_special_chars() {
        assert_eq!(
            quote_qualified("\"my-schema\".\"weird name\"").unwrap(),
            "\"my-schema\".\"weird name\""
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
    fn split_qualified_bare_name_defaults_to_dbo() {
        assert_eq!(
            split_qualified("users").unwrap(),
            ("dbo".into(), "users".into())
        );
    }

    #[test]
    fn split_qualified_two_part() {
        assert_eq!(
            split_qualified("myschema.users").unwrap(),
            ("myschema".into(), "users".into())
        );
    }

    #[test]
    fn split_qualified_unwraps_quoted_segments() {
        assert_eq!(
            split_qualified("\"my-schema\".\"My Table\"").unwrap(),
            ("my-schema".into(), "My Table".into())
        );
    }

    #[test]
    fn split_qualified_rejects_three_segments() {
        assert!(matches!(
            split_qualified("a.b.c"),
            Err(IdentifierError::TooManySegments { .. })
        ));
    }
}
