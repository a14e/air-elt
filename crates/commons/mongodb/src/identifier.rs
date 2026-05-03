//! Mongo collection / database identifier validation.
//!
//! Mongo allows a wider character class than SQL bare identifiers
//! (e.g. dots and dollars are forbidden by the server, but hyphens
//! and unicode are allowed). For our config surface we keep the rule
//! simple and aligned with the rest of the project: collection /
//! database names accepted by `[[sources]] config = { database = ... }`
//! and flow `from = "..."` must satisfy
//! `air_elt_commons::identifier::validate_segment` — i.e. ASCII
//! `[A-Za-z0-9_$]` only, no dots, no whitespace.
//!
//! Operators with truly exotic collection names can rename them or
//! file an issue; carrying the full Mongo grammar through the config
//! parser would require quoting which we don't want in MVP.

pub use air_elt_commons::identifier::{IdentifierError, validate_segment};

/// Validate a Mongo database or collection name. Returns the input
/// unchanged on success.
pub fn validate_name(name: &str) -> Result<&str, IdentifierError> {
    validate_segment(name)?;
    Ok(name)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn plain_name_ok() {
        assert!(validate_name("users").is_ok());
        assert!(validate_name("user_v2").is_ok());
        assert!(validate_name("$cmd").is_ok());
    }

    #[test]
    fn dot_rejected() {
        assert!(validate_name("foo.bar").is_err());
    }

    #[test]
    fn empty_rejected() {
        assert!(validate_name("").is_err());
    }

    #[test]
    fn whitespace_rejected() {
        assert!(validate_name("foo bar").is_err());
    }
}
