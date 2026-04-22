use thiserror::Error;

#[derive(Debug, Error)]
pub enum SecretError {
    #[error("env var {var:?} is not set")]
    Missing { var: String },
}

/// Resolve a `$ENV_VAR` prefix, or pass the string through unchanged.
///
/// Unlike `core::config::secrets_ref`, this version only understands plain env
/// indirection — it's meant for runtime callsites that don't carry the config
/// secrets map around (e.g. CLI flags). Config-time resolution lives in `core`.
pub fn resolve(raw: &str) -> Result<String, SecretError> {
    let Some(name) = raw.strip_prefix('$') else {
        return Ok(raw.to_string());
    };
    std::env::var(name).map_err(|_| SecretError::Missing {
        var: name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_pass_through() {
        assert_eq!(resolve("hello").unwrap(), "hello");
    }

    #[test]
    fn env_lookup() {
        // Why: edition 2024 marks `set_var` unsafe. In a cargo-test harness each
        // `#[test]` runs on its own thread within a serial test phase for this
        // crate's tiny suite — no concurrent readers of this specific var exist.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("AIR_ELT_TEST_COMMONS_SECRET", "ok");
        }
        assert_eq!(resolve("$AIR_ELT_TEST_COMMONS_SECRET").unwrap(), "ok");
    }

    #[test]
    fn missing_env_errors() {
        let err = resolve("$AIR_ELT_SHOULD_NOT_EXIST_X_Y_Z").unwrap_err();
        assert!(matches!(err, SecretError::Missing { .. }));
    }
}
