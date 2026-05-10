//! Shorthand grammar for the `mapping = [...]` array.
//!
//! Three compact forms in addition to the existing
//! long-form `{ from, to, truncate?, default? }` table:
//!
//! - `"NAME"` — short identity. Equivalent to
//!   `{ from = "NAME", to = "NAME" }`.
//! - `"FROM:TO"` — short rename. Equivalent to
//!   `{ from = "FROM", to = "TO" }`. Direction is `from:to`.
//! - `"*"` (or `"*:*"`) — wildcard expansion. Resolved at validation
//!   time against the sink schema (preferred) or source schema. When
//!   both sides are schemaless and the source supports raw-mode
//!   emission, falls back to a per-row passthrough.
//! - `"*:NAME"` — JSON auto-pack. All source fields are packed into
//!   one `Value::Json` placed in sink column `NAME`.
//!
//! Forbidden shapes (rejected by [`parse`] with
//! [`ConfigError::Invalid`]):
//!
//! - empty / whitespace-only,
//! - leading or trailing whitespace (no trim — operator typos surface
//!   loudly rather than silently round-tripping through normalisation),
//! - missing `from` or `to` around the `:` separator,
//! - more than one `:` separator,
//! - `"field:*"` — broadcasting a single source to every sink column
//!   is ambiguous and explicitly disallowed.
//!
//! `"NAME"` and the two halves of `"FROM:TO"` are validated through
//! [`FieldPath`] so dotted paths (`"address.city"`) work for connectors
//! that emit nested documents (Mongo). Names are not normalised: the
//! string the operator wrote is what reaches the rest of the pipeline.

use crate::error::ConfigError;
use crate::mapping::path::FieldPath;

/// Parsed shorthand result. The downstream
/// `crate::mapping::column::build` step folds this into one of the
/// `ColumnMapping` variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedShorthand {
    /// Rename form: `"FROM:TO"` ≡ `{ from = FROM, to = TO }`. The
    /// identity short form (`"NAME"`) is also represented here with
    /// `from == to` — there is no separate `Field` variant.
    Renamed { from: String, to: String },
    /// Wildcard expansion: `"*"` or `"*:*"`.
    Wildcard,
    /// JSON auto-pack: `"*:NAME"`. The single sink column `NAME` will
    /// receive a `Value::Json` containing every source field.
    Body { to: String },
}

/// Parse a single shorthand string. Returns [`ConfigError::Invalid`]
/// with the offending input quoted in the message on every reject case
/// listed in the module docs. The function never trims — whitespace is
/// rejected, including unicode whitespace.
pub fn parse(s: &str) -> Result<ParsedShorthand, ConfigError> {
    if s.is_empty() {
        return Err(invalid(s, "shorthand mapping rule is empty"));
    }
    if has_whitespace(s) {
        return Err(invalid(
            s,
            "shorthand mapping rule contains whitespace (no trim — fix the source)",
        ));
    }

    if s == "*" || s == "*:*" {
        return Ok(ParsedShorthand::Wildcard);
    }

    let mut parts = s.split(':');
    // SAFETY: split() always yields at least one part; non-empty `s`
    // means that part is non-empty unless `s` starts with `:`, which
    // we surface as an empty `from` half below.
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    let extra = parts.next();

    if extra.is_some() || s.contains("::") {
        return Err(invalid(
            s,
            "shorthand mapping rule must contain at most one ':' separator",
        ));
    }

    let Some(second) = second else {
        // No colon at all → identity form (from == to).
        return validate_field_name(s).map(|_| ParsedShorthand::Renamed {
            from: s.to_string(),
            to: s.to_string(),
        });
    };

    // Both halves must be non-empty (rejects ":x" and "x:").
    if first.is_empty() {
        return Err(invalid(
            s,
            "shorthand mapping rule has an empty 'from' half before ':'",
        ));
    }
    if second.is_empty() {
        return Err(invalid(
            s,
            "shorthand mapping rule has an empty 'to' half after ':'",
        ));
    }

    // `*:NAME` → JSON auto-pack. `NAME` is validated as a single
    // identifier path; nested paths are not allowed for the json-pack
    // sink column because the auto-pack target is intentionally a
    // top-level column carrying the whole JSON document.
    if first == "*" {
        if second == "*" {
            // already handled above; re-checking keeps the branch tidy.
            return Ok(ParsedShorthand::Wildcard);
        }
        validate_field_name(second)?;
        return Ok(ParsedShorthand::Body {
            to: second.to_string(),
        });
    }

    // `field:*` is explicitly forbidden — broadcasting a single
    // source field to every sink column is ambiguous.
    if second == "*" {
        return Err(invalid(
            s,
            "shorthand 'field:*' is forbidden — wildcard is only valid on the source side",
        ));
    }

    validate_field_name(first)?;
    validate_field_name(second)?;
    Ok(ParsedShorthand::Renamed {
        from: first.to_string(),
        to: second.to_string(),
    })
}

/// Run [`FieldPath::parse`] for its identifier-character validation;
/// the parsed path itself is discarded — we only keep the original
/// string in the result. Surfaces parse errors as `ConfigError::Invalid`
/// so the operator sees a single error type for all shorthand rejects.
fn validate_field_name(s: &str) -> Result<(), ConfigError> {
    FieldPath::parse(s)
        .map(|_| ())
        .map_err(|source| ConfigError::Invalid {
            reason: format!("invalid mapping shorthand {s:?}: {source}"),
        })
}

fn invalid(input: &str, msg: &str) -> ConfigError {
    ConfigError::Invalid {
        reason: format!("invalid mapping shorthand {input:?}: {msg}"),
    }
}

/// Reject any whitespace, including unicode (e.g. NBSP `U+00A0`,
/// thin space `U+2009`). `char::is_whitespace` covers the full
/// Unicode `WSpace=Y` set per the language spec.
fn has_whitespace(s: &str) -> bool {
    s.chars().any(char::is_whitespace)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    /// Exhaustive list of strings that must be rejected by `parse`.
    /// Kept as a `const` so additions are visible at a glance and the
    /// test loop fails per-case with the exact offending input.
    const REJECTS: &[&str] = &[
        "",
        "   ",
        ":",
        ":x",
        "x:",
        "x:y:z",
        "x::y",
        "field:*",
        "*:",
        ":*",
        " a",
        "a ",
        " a:b ",
        "\u{00A0}a",
    ];

    #[test]
    fn field_identity_forms() {
        for input in ["name", "created_at", "address.city"] {
            let parsed = parse(input).unwrap_or_else(|e| panic!("{input:?}: {e}"));
            assert_eq!(
                parsed,
                ParsedShorthand::Renamed {
                    from: input.to_string(),
                    to: input.to_string(),
                }
            );
        }
    }

    #[test]
    fn renamed_form() {
        let parsed = parse("a:b").unwrap();
        assert_eq!(
            parsed,
            ParsedShorthand::Renamed {
                from: "a".into(),
                to: "b".into(),
            }
        );
    }

    #[test]
    fn wildcard_forms() {
        assert_eq!(parse("*").unwrap(), ParsedShorthand::Wildcard);
        assert_eq!(parse("*:*").unwrap(), ParsedShorthand::Wildcard);
    }

    #[test]
    fn body_form() {
        let parsed = parse("*:body").unwrap();
        assert_eq!(parsed, ParsedShorthand::Body { to: "body".into() });
    }

    #[test]
    fn rejects_table_is_exhaustive() {
        for &input in REJECTS {
            let err = match parse(input) {
                Err(e) => e,
                Ok(parsed) => {
                    panic!("expected {input:?} to be rejected, but parsed as {parsed:?}",)
                }
            };
            let msg = err.to_string();
            // Why: every reject must name the offending input verbatim
            // in the error message — the operator should not have to
            // guess which entry tripped the parser.
            assert!(
                msg.contains(&format!("{input:?}")),
                "error for {input:?} should quote the input; got {msg:?}",
            );
        }
    }
}
