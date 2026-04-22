//! `${VAR}` / `${VAR:default}` expansion over raw config text.
//!
//! Why string-level and not AST-walking: it lets operators put references
//! anywhere (URL bodies, table names, any field) without the loader needing to
//! know the shape of the config first. The two-parse approach in `loader.rs`
//! extracts the `[secrets]` table from the *raw* TOML once, then expands, then
//! parses for real.

use std::collections::BTreeMap;

use once_cell::sync::Lazy;
use regex::{Captures, Regex};
use tracing::debug;

use crate::error::ConfigError;

// ${NAME} or ${NAME:default}. NAME is POSIX-ish identifier; default extends
// to the closing `}` and may be empty.
static REF_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$\{([a-zA-Z_][0-9a-zA-Z_]*)(?::([^}]*))?\}").unwrap());

/// Resolve every `${VAR}` / `${VAR:default}` in `raw`.
///
/// Lookup order: process env → `secrets` map → default clause → error.
/// Secrets are literals: they are not themselves expanded, by design — it
/// keeps the resolver single-pass and cycle-free for MVP. Operators who need
/// chained lookups should use env vars directly.
pub fn expand(raw: &str, secrets: &BTreeMap<String, String>) -> Result<String, ConfigError> {
    let mut err: Option<ConfigError> = None;
    let result = REF_RE.replace_all(raw, |caps: &Captures| {
        let name = &caps[1];
        let default = caps.get(2).map(|m| m.as_str());
        match std::env::var(name)
            .ok()
            .or_else(|| secrets.get(name).cloned())
        {
            Some(value) => {
                debug!(%name, "resolved config reference from env/secrets");
                value
            }
            None => match default {
                Some(d) if !d.is_empty() => {
                    debug!(%name, default = %d, "config reference fell back to default");
                    d.to_string()
                }
                _ => {
                    if err.is_none() {
                        err = Some(ConfigError::UnresolvedReference {
                            name: name.to_string(),
                        });
                    }
                    String::new()
                }
            },
        }
    });
    if let Some(e) = err {
        return Err(e);
    }
    Ok(result.into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_env(name: &str, value: &str) {
        // Why: edition 2024 marks `set_var` unsafe. Tests in this module use
        // namespaced `AIR_ELT_TEST_*` names so no concurrent reader touches them.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var(name, value);
        }
    }

    #[test]
    fn literal_passthrough() {
        let secrets = BTreeMap::new();
        assert_eq!(expand("plain text", &secrets).unwrap(), "plain text");
    }

    #[test]
    fn resolves_from_env() {
        set_env("AIR_ELT_TEST_EXPAND_X", "from-env");
        let out = expand(r#"url = "${AIR_ELT_TEST_EXPAND_X}""#, &BTreeMap::new()).unwrap();
        assert_eq!(out, r#"url = "from-env""#);
    }

    #[test]
    fn resolves_from_secrets_when_env_missing() {
        let mut secrets = BTreeMap::new();
        secrets.insert("MY_KEY".into(), "from-secrets".into());
        let out = expand(r#"k = "${MY_KEY}""#, &secrets).unwrap();
        assert_eq!(out, r#"k = "from-secrets""#);
    }

    #[test]
    fn env_takes_precedence_over_secrets() {
        set_env("AIR_ELT_TEST_PRECEDENCE", "env-wins");
        let mut secrets = BTreeMap::new();
        secrets.insert("AIR_ELT_TEST_PRECEDENCE".into(), "secret-loses".into());
        let out = expand(r#"k = "${AIR_ELT_TEST_PRECEDENCE}""#, &secrets).unwrap();
        assert_eq!(out, r#"k = "env-wins""#);
    }

    #[test]
    fn default_used_when_not_found() {
        let out = expand(
            r#"k = "${AIR_ELT_SHOULD_NOT_EXIST_Q:fallback}""#,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(out, r#"k = "fallback""#);
    }

    #[test]
    fn empty_default_is_treated_as_no_default() {
        let err = expand(r#"k = "${AIR_ELT_SHOULD_NOT_EXIST_R:}""#, &BTreeMap::new()).unwrap_err();
        assert!(matches!(err, ConfigError::UnresolvedReference { .. }));
    }

    #[test]
    fn missing_reference_errors() {
        let err = expand(r#"k = "${AIR_ELT_SHOULD_NOT_EXIST_Z}""#, &BTreeMap::new()).unwrap_err();
        assert!(
            matches!(err, ConfigError::UnresolvedReference { name } if name == "AIR_ELT_SHOULD_NOT_EXIST_Z")
        );
    }

    #[test]
    fn multiple_references_in_one_line() {
        set_env("AIR_ELT_TEST_A", "aaa");
        set_env("AIR_ELT_TEST_B", "bbb");
        let out = expand(
            r#"url = "${AIR_ELT_TEST_A}://${AIR_ELT_TEST_B}/x""#,
            &BTreeMap::new(),
        )
        .unwrap();
        assert_eq!(out, r#"url = "aaa://bbb/x""#);
    }
}
