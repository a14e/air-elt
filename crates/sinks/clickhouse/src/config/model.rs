use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_commons::interval;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

/// Pool-setting keys present in [`air_elt_commons::pool_settings::PoolSettings`]
/// that reqwest does **not** expose. If a user specifies any of these in a
/// ClickHouse sink config block they are silently ignored, which is
/// confusing. We reject them early with a clear error message instead.
const UNSUPPORTED_POOL_KEYS: &[&str] = &["acquire-timeout", "max-lifetime", "min-connections"];

/// Returns an error if `table` contains any pool key that reqwest does not
/// support. The error names the offending field and explains which fields are
/// accepted.
fn reject_unsupported_pool_keys(
    table: &toml::Table,
    connector_name: &str,
) -> Result<(), ConfigError> {
    for key in UNSUPPORTED_POOL_KEYS {
        if table.contains_key(*key) {
            return Err(ConfigError::Invalid {
                reason: format!(
                    "ClickHouse sink '{connector_name}': pool field '{key}' is not supported by \
                     the reqwest HTTP client — remove this field. \
                     Supported pool fields: connect-timeout, request-timeout, idle-timeout, max-connections.",
                ),
            });
        }
    }
    Ok(())
}

/// Compression algorithm applied to INSERT bodies. Mirrors
/// [`air_elt_commons_clickhouse::client::ChCompression`] — duplicated
/// here only because the sink-config struct must be serde-aware
/// (`kebab-case`) and the helper crate's enum stays a plain Rust enum
/// for ergonomic use by the client code.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChCompressionKind {
    None,
    /// Standard LZ4 frame format. Decoded by ClickHouse server-side
    /// via `Content-Encoding: lz4`.
    #[default]
    Lz4,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChSinkConfig {
    /// HTTP endpoint URL, e.g. `http://localhost:8123`.
    pub url: String,
    /// Default database name. Used as `X-ClickHouse-Database` and
    /// when a flow's `to` is given as a bare table name.
    pub database: String,
    /// CH user. Required at deserialize time — omitting the field in
    /// TOML produces a clear `missing field 'user'` error before any
    /// I/O. Use `user = "default"` together with `password = ""` for
    /// authless CH instances (CH treats matching empty passwords as
    /// accept). Do NOT mark with `#[serde(default)]`: an empty default
    /// would silently re-introduce the cryptic 403 from CH 24.3+ that
    /// rejects the `default` user from network endpoints when no
    /// `<networks>` override is configured.
    pub user: String,
    /// CH password matching `user`. Required at deserialize time.
    /// Use `""` for authless setups; see the comment on `user`.
    pub password: String,
    /// Body compression for INSERTs. Defaults to `lz4`.
    #[serde(default)]
    pub compression: ChCompressionKind,
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub connect_timeout: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub idle_timeout: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub request_timeout: Option<Duration>,
    #[serde(default)]
    pub max_connections: Option<u32>,
}

/// Manual `Default` (we cannot derive — `url` / `database` have no
/// sensible defaults, but we want `..Default::default()` ergonomics in
/// tests that set those two fields explicitly). The credential defaults
/// match the authless variant documented on `ChSinkConfig::user`.
///
/// This `Default` impl is **not** wired into serde — `[serde(default)]`
/// stays off `user` / `password` so a TOML config that omits either
/// field fails to parse with a clear `missing field 'user'`
/// diagnostic, instead of silently falling through to the empty-string
/// default and surfacing a runtime 403 from CH 24.3+.
impl Default for ChSinkConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            database: String::new(),
            user: "default".to_string(),
            password: String::new(),
            compression: ChCompressionKind::default(),
            connect_timeout: None,
            idle_timeout: None,
            request_timeout: None,
            max_connections: None,
        }
    }
}

impl TryFrom<&ComponentConfig> for ChSinkConfig {
    type Error = ConfigError;

    fn try_from(cfg: &ComponentConfig) -> Result<Self, Self::Error> {
        reject_unsupported_pool_keys(&cfg.config, &cfg.name)?;
        cfg.config
            .clone()
            .try_into::<Self>()
            .map_err(|source| ConfigError::TomlParse {
                path: std::path::PathBuf::from(format!("<inline:{}>", cfg.name)),
                source,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_component_config(toml_str: &str) -> ComponentConfig {
        let table: toml::Table = toml::from_str(toml_str).expect("valid toml");
        ComponentConfig {
            name: "ch_test".to_string(),
            kind: "clickhouse".to_string(),
            config: table,
        }
    }

    fn assert_unsupported_field_error(result: Result<ChSinkConfig, ConfigError>, field: &str) {
        match result {
            Err(ConfigError::Invalid { reason }) => {
                assert!(
                    reason.contains(field),
                    "error message should mention the offending field '{field}', got: {reason}"
                );
                assert!(
                    reason.contains("not supported"),
                    "error message should say 'not supported', got: {reason}"
                );
                assert!(
                    reason.contains("connect-timeout"),
                    "error message should list supported fields, got: {reason}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    #[test]
    fn acquire_timeout_is_rejected() {
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               acquire-timeout = "10s""#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        assert_unsupported_field_error(result, "acquire-timeout");
    }

    #[test]
    fn max_lifetime_is_rejected() {
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               max-lifetime = "30m""#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        assert_unsupported_field_error(result, "max-lifetime");
    }

    #[test]
    fn min_connections_is_rejected() {
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               min-connections = 1"#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        assert_unsupported_field_error(result, "min-connections");
    }

    #[test]
    fn multiple_unsupported_fields_first_one_reported() {
        // The validator stops at the first offending key.
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               acquire-timeout = "5s"
               max-lifetime = "1h"
               min-connections = 2"#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        // Any of the three keys triggers the error — test that we get one.
        match result {
            Err(ConfigError::Invalid { reason }) => {
                assert!(
                    reason.contains("acquire-timeout")
                        || reason.contains("max-lifetime")
                        || reason.contains("min-connections"),
                    "error should name one of the unsupported fields, got: {reason}"
                );
            }
            other => panic!("expected ConfigError::Invalid, got: {other:?}"),
        }
    }

    #[test]
    fn valid_supported_fields_accepted() {
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               user = "air"
               password = "air"
               connect-timeout = "3s"
               request-timeout = "60s"
               idle-timeout = "2m"
               max-connections = 10"#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        assert!(
            result.is_ok(),
            "valid config should be accepted: {result:?}"
        );
        let parsed = result.expect("already checked");
        assert_eq!(parsed.url, "http://localhost:8123");
        assert_eq!(parsed.database, "default");
        assert_eq!(parsed.user, "air");
        assert_eq!(parsed.password, "air");
        assert_eq!(parsed.max_connections, Some(10));
    }

    #[test]
    fn missing_user_field_is_rejected_with_clear_diagnostic() {
        // CH 24.3+ rejects the `default` user from network endpoints
        // by default; moving the credential check to serde gives users
        // a clear "missing field `user`" diagnostic before any I/O,
        // instead of a cryptic runtime 403.
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               password = """#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        match result {
            Err(ConfigError::TomlParse { source, .. }) => {
                let message = source.to_string();
                assert!(
                    message.contains("user"),
                    "error message should mention the missing 'user' field, got: {message}"
                );
            }
            other => panic!("expected ConfigError::TomlParse, got: {other:?}"),
        }
    }

    #[test]
    fn missing_password_field_is_rejected_with_clear_diagnostic() {
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               user = "default""#,
        );
        let result = ChSinkConfig::try_from(&cfg);
        match result {
            Err(ConfigError::TomlParse { source, .. }) => {
                let message = source.to_string();
                assert!(
                    message.contains("password"),
                    "error message should mention the missing 'password' field, got: {message}"
                );
            }
            other => panic!("expected ConfigError::TomlParse, got: {other:?}"),
        }
    }

    #[test]
    fn authless_empty_password_is_accepted() {
        // Authless CH (with `<networks><ip>::/0</ip></networks>` open
        // for default-no-password): operator writes
        // `user = "default", password = ""`. Both fields must still
        // be present in the TOML.
        let cfg = make_component_config(
            r#"url = "http://localhost:8123"
               database = "default"
               user = "default"
               password = """#,
        );
        let parsed = ChSinkConfig::try_from(&cfg).expect("authless config should parse");
        assert_eq!(parsed.user, "default");
        assert_eq!(parsed.password, "");
    }
}
