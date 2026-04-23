use thiserror::Error;

use air_elt_core::error::RuntimeError;

#[derive(Debug, Error)]
pub enum IdentifierError {
    #[error("identifier segment is empty")]
    EmptySegment,
    #[error("identifier segment {segment:?} contains unsupported character {ch:?}")]
    UnsupportedChar { segment: String, ch: char },
}

// Why: connectors compose SQL via these helpers and then map errors through `?`
// into `RuntimeResult`. A dedicated From impl keeps call sites clean — no
// per-file wrapper closures.
impl From<IdentifierError> for RuntimeError {
    fn from(err: IdentifierError) -> Self {
        RuntimeError::Other(err.to_string())
    }
}

/// Double-quote a single postgres identifier and escape any internal `"`
/// by doubling it.
///
/// Examples:
/// - `users` → `"users"`
/// - `he"llo` → `"he""llo"`
/// - empty string → `""` (not an error — callers with semantic rules
///   should reject it themselves)
pub fn quote_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    out.push('"');
    for ch in name.chars() {
        if ch == '"' {
            out.push_str("\"\"");
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

/// Quote a dotted identifier like `schema.table` → `"schema"."table"`.
///
/// The dot is treated as the segment separator; it is emitted *outside* the
/// quotes. Each segment is validated to match `[A-Za-z0-9_$]+` — if any other
/// character is present we return `IdentifierError::UnsupportedChar`, since
/// the raw form is ambiguous (we can't tell a literal dot inside a name from
/// a separator).
pub fn quote_qualified(name: &str) -> Result<String, IdentifierError> {
    let segments: Vec<&str> = name.split('.').collect();
    let mut out = String::with_capacity(name.len() + segments.len() * 2);
    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            return Err(IdentifierError::EmptySegment);
        }
        for ch in segment.chars() {
            if !is_bare_ident_char(ch) {
                return Err(IdentifierError::UnsupportedChar {
                    segment: (*segment).to_string(),
                    ch,
                });
            }
        }
        if i > 0 {
            out.push('.');
        }
        out.push_str(&quote_ident(segment));
    }
    Ok(out)
}

fn is_bare_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

/// Convenience: quote a list of column names into a comma-joined string.
pub fn quote_columns(names: &[String]) -> Result<String, IdentifierError> {
    let mut out = String::new();
    for (i, name) in names.iter().enumerate() {
        for ch in name.chars() {
            if !is_bare_ident_char(ch) {
                return Err(IdentifierError::UnsupportedChar {
                    segment: name.clone(),
                    ch,
                });
            }
        }
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&quote_ident(name));
    }
    Ok(out)
}

/// Split a dotted identifier into `(schema, table)`. A bare name falls back
/// to `public`. Used at `information_schema` query sites where we must pass
/// schema and table separately as bound parameters.
pub fn split_qualified(name: &str) -> (String, String) {
    match name.rsplit_once('.') {
        Some((schema, table)) => (schema.to_string(), table.to_string()),
        None => ("public".to_string(), name.to_string()),
    }
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
    fn quote_qualified_three_segments() {
        assert_eq!(
            quote_qualified("db.schema.table").unwrap(),
            "\"db\".\"schema\".\"table\""
        );
    }

    #[test]
    fn quote_qualified_rejects_special_chars() {
        let err = quote_qualified("foo bar").unwrap_err();
        assert!(matches!(err, IdentifierError::UnsupportedChar { .. }));
        let err = quote_qualified("foo-bar").unwrap_err();
        assert!(matches!(err, IdentifierError::UnsupportedChar { .. }));
    }

    #[test]
    fn quote_qualified_rejects_empty_segment() {
        let err = quote_qualified(".table").unwrap_err();
        assert!(matches!(err, IdentifierError::EmptySegment));
        let err = quote_qualified("schema.").unwrap_err();
        assert!(matches!(err, IdentifierError::EmptySegment));
    }

    #[test]
    fn quote_columns_comma_joined() {
        let cols = vec!["id".into(), "name".into()];
        assert_eq!(quote_columns(&cols).unwrap(), "\"id\", \"name\"");
    }
}
