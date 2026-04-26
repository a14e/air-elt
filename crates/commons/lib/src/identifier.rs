//! Db-agnostic identifier validation. Each backend defines its own quote
//! character — see `air-elt-commons-pg` and `air-elt-commons-mysql` —
//! but the *validation* rules (allowed character class, segment-must-be-
//! non-empty) and the dotted-path parser are identical, so they live here.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum IdentifierError {
    #[error("identifier segment is empty")]
    EmptySegment,
    #[error("identifier segment {segment:?} contains unsupported character {ch:?}")]
    UnsupportedChar { segment: String, ch: char },
    #[error("identifier {value:?} has {got} dotted segments; backend allows at most {max}")]
    TooManySegments {
        value: String,
        got: usize,
        max: usize,
    },
    #[error("identifier {value:?} has an unterminated quoted segment")]
    UnterminatedQuote { value: String },
    #[error(
        "identifier {value:?} has unexpected character {ch:?} after a quoted segment; expected `.` or end"
    )]
    UnexpectedAfterQuote { value: String, ch: char },
}

// `From<IdentifierError> for RuntimeError` lives in `air-elt-core::error`.
// commons/lib has no project-internal dependencies (see project-conventions
// → Commons isolation).

/// One dotted component of a qualified identifier. `quoted = true` means
/// the user wrapped it in the dialect's quote char and `value` holds the
/// already-unescaped contents (`""` / ` `` ` collapsed to a single quote).
/// Quoted segments bypass `validate_segment`: the user has explicitly opted
/// into raw characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSegment {
    pub value: String,
    pub quoted: bool,
}

/// `[A-Za-z0-9_$]` — the bare-identifier character class accepted by every
/// SQL dialect we currently target without surrounding quotes.
pub fn is_bare_ident_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || ch == '_' || ch == '$'
}

/// Reject empty or unsupported-character segments. Used by every dialect's
/// `quote_qualified` / `quote_columns` to keep behaviour identical.
pub fn validate_segment(segment: &str) -> Result<(), IdentifierError> {
    if segment.is_empty() {
        return Err(IdentifierError::EmptySegment);
    }
    for ch in segment.chars() {
        if !is_bare_ident_char(ch) {
            return Err(IdentifierError::UnsupportedChar {
                segment: segment.to_string(),
                ch,
            });
        }
    }
    Ok(())
}

/// Split a possibly-quoted, dotted identifier into its segments.
///
/// Each segment is either bare (will be validated by callers via
/// `validate_segment`) or quoted in `quote` (passed through as raw text;
/// SQL-style doubled quotes inside are collapsed). A `.` *inside* a quoted
/// segment is part of the name; outside it splits segments.
///
/// The returned `quoted` flag tells the caller whether the user opted into
/// raw characters — quoted segments skip the bare-character validation.
pub fn parse_qualified(input: &str, quote: char) -> Result<Vec<ParsedSegment>, IdentifierError> {
    let mut segments = Vec::new();
    let mut buf = String::new();
    let mut quoted = false;
    let mut in_quoted = false;
    let mut after_quoted = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quoted {
            if ch == quote {
                if chars.peek() == Some(&quote) {
                    chars.next();
                    buf.push(quote);
                } else {
                    in_quoted = false;
                    after_quoted = true;
                }
            } else {
                buf.push(ch);
            }
            continue;
        }

        if ch == '.' {
            segments.push(ParsedSegment {
                value: std::mem::take(&mut buf),
                quoted,
            });
            quoted = false;
            after_quoted = false;
            continue;
        }

        if after_quoted {
            return Err(IdentifierError::UnexpectedAfterQuote {
                value: input.to_string(),
                ch,
            });
        }

        if ch == quote && buf.is_empty() {
            in_quoted = true;
            quoted = true;
            continue;
        }

        buf.push(ch);
    }

    if in_quoted {
        return Err(IdentifierError::UnterminatedQuote {
            value: input.to_string(),
        });
    }

    segments.push(ParsedSegment { value: buf, quoted });
    Ok(segments)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_dotted() {
        let segs = parse_qualified("schema.table", '"').unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].value, "schema");
        assert!(!segs[0].quoted);
        assert_eq!(segs[1].value, "table");
    }

    #[test]
    fn parses_quoted_segments_with_inner_dot() {
        let segs = parse_qualified("\"weird.name\".\"t\"", '"').unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].value, "weird.name");
        assert!(segs[0].quoted);
        assert_eq!(segs[1].value, "t");
        assert!(segs[1].quoted);
    }

    #[test]
    fn quoted_doubled_quote_unescapes() {
        let segs = parse_qualified("\"he\"\"llo\"", '"').unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].value, "he\"llo");
    }

    #[test]
    fn mixed_quoted_and_bare() {
        let segs = parse_qualified("schema.\"My Table\"", '"').unwrap();
        assert_eq!(segs.len(), 2);
        assert!(!segs[0].quoted);
        assert!(segs[1].quoted);
        assert_eq!(segs[1].value, "My Table");
    }

    #[test]
    fn unterminated_quote_errors() {
        let err = parse_qualified("\"oops", '"').unwrap_err();
        assert!(matches!(err, IdentifierError::UnterminatedQuote { .. }));
    }

    #[test]
    fn junk_after_close_quote_errors() {
        let err = parse_qualified("\"a\"x", '"').unwrap_err();
        assert!(matches!(err, IdentifierError::UnexpectedAfterQuote { .. }));
    }

    #[test]
    fn backtick_quote_for_mysql() {
        let segs = parse_qualified("`a.b`.c", '`').unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].value, "a.b");
        assert!(segs[0].quoted);
        assert_eq!(segs[1].value, "c");
        assert!(!segs[1].quoted);
    }
}
