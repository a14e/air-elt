//! Parsing of human-friendly boolean flags. Used for env-var driven toggles
//! (`AIR_ELT_SYNC_LOGGING`, `AIR_ELT_JSON_LOGGING`, etc.) and any other
//! string-typed boolean knob in the project. Centralised so every call site
//! accepts the same vocabulary.

/// Parse a string as a boolean. Recognises (case- and whitespace-insensitive):
/// - truthy: `true`, `1`, `t`, `y`, `yes`
/// - falsy:  `false`, `0`, `f`, `n`, `no`
///
/// Returns `None` for anything unrecognised so callers can distinguish
/// "explicitly false" from "unknown value".
pub fn parse(value: &str) -> Option<bool> {
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("true")
        || trimmed == "1"
        || trimmed.eq_ignore_ascii_case("t")
        || trimmed.eq_ignore_ascii_case("y")
        || trimmed.eq_ignore_ascii_case("yes")
    {
        Some(true)
    } else if trimmed.eq_ignore_ascii_case("false")
        || trimmed == "0"
        || trimmed.eq_ignore_ascii_case("f")
        || trimmed.eq_ignore_ascii_case("n")
        || trimmed.eq_ignore_ascii_case("no")
    {
        Some(false)
    } else {
        None
    }
}

/// Read env var `key` and parse it via [`parse`]. Returns `default` when the
/// variable is unset or holds an unrecognised value.
pub fn from_env(key: &str, default: bool) -> bool {
    std::env::var(key)
        .ok()
        .as_deref()
        .and_then(parse)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{from_env, parse};

    #[test]
    fn parse_truthy_variants() {
        for value in [
            "true", "TRUE", "True", "tRuE", " true ", "1", " 1 ", "t", "T", "y", "Y", "yes", "YES",
        ] {
            assert_eq!(parse(value), Some(true), "expected true for {value:?}");
        }
    }

    #[test]
    fn parse_falsy_variants() {
        for value in [
            "false", "FALSE", "False", " false ", "0", " 0 ", "f", "F", "n", "N", "no", "NO",
        ] {
            assert_eq!(parse(value), Some(false), "expected false for {value:?}");
        }
    }

    #[test]
    fn parse_returns_none_for_unknown() {
        for value in [
            "", "  ", "maybe", "2", "yep", "nope", "off", "on", "tru", "ye",
        ] {
            assert_eq!(parse(value), None, "expected None for {value:?}");
        }
    }

    // Each test owns a unique env-var name so the parallel test runner doesn't
    // observe writes from a sibling test mid-read. `set_var`/`remove_var` are
    // unsafe under Rust 2024 because they're racy with `var`; the project
    // denies `unsafe_code` workspace-wide, so we narrow the allow here.
    #[allow(unsafe_code)]
    #[test]
    fn from_env_returns_default_when_unset() {
        let key = "AIR_ELT_BOOL_FLAG_TEST_UNSET";
        unsafe {
            std::env::remove_var(key);
        }
        assert!(from_env(key, true));
        assert!(!from_env(key, false));
    }

    #[allow(unsafe_code)]
    #[test]
    fn from_env_parses_recognised_value() {
        let key = "AIR_ELT_BOOL_FLAG_TEST_RECOGNISED";
        unsafe {
            std::env::set_var(key, "yes");
        }
        assert!(from_env(key, false));
        unsafe {
            std::env::set_var(key, "no");
        }
        assert!(!from_env(key, true));
        unsafe {
            std::env::remove_var(key);
        }
    }

    #[allow(unsafe_code)]
    #[test]
    fn from_env_returns_default_for_garbage() {
        let key = "AIR_ELT_BOOL_FLAG_TEST_GARBAGE";
        unsafe {
            std::env::set_var(key, "definitely-not-a-bool");
        }
        assert!(from_env(key, true));
        assert!(!from_env(key, false));
        unsafe {
            std::env::remove_var(key);
        }
    }
}
