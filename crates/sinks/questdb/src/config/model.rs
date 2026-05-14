//! QuestDB sink configuration.
//!
//! Single transport: `url` — pg-wire control plane (`postgres://...`).
//! All writes, schema introspection, and dry-run probes flow over this
//! connection.
//!
//! `deny_unknown_fields` rejects every key the sink does not understand
//! — surfaces operator typos (`acquire-timeout`, `max-lifetime`, ...).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_core::config::interval;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct QuestDbSinkConfig {
    /// pg-wire URL. Required.
    pub url: String,

    // pg-wire pool tunables.
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
    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub min_connections: Option<u32>,
}

impl QuestDbSinkConfig {
    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        if !self.url.starts_with("postgres://") && !self.url.starts_with("postgresql://") {
            return Err(ConfigError::Invalid {
                reason: format!(
                    "QuestDB sink {name:?}: url must start with `postgres://` or `postgresql://`"
                ),
            });
        }
        Ok(())
    }
}

impl TryFrom<&ComponentConfig> for QuestDbSinkConfig {
    type Error = ConfigError;

    fn try_from(cfg: &ComponentConfig) -> Result<Self, Self::Error> {
        let parsed: Self =
            cfg.config
                .clone()
                .try_into::<Self>()
                .map_err(|source| ConfigError::TomlParse {
                    path: std::path::PathBuf::from(format!("<inline:{}>", cfg.name)),
                    source,
                })?;
        parsed.validate(&cfg.name)?;
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(toml_str: &str) -> ComponentConfig {
        let table: toml::Table = toml::from_str(toml_str).expect("valid toml");
        ComponentConfig {
            name: "qdb".to_string(),
            kind: "questdb".to_string(),
            config: table,
        }
    }

    #[test]
    fn minimal_config_accepted() {
        let cfg = make(r#"url = "postgres://admin:quest@localhost:8812/qdb""#);
        let parsed = QuestDbSinkConfig::try_from(&cfg).expect("ok");
        assert!(parsed.url.starts_with("postgres://"));
    }

    #[test]
    fn full_config_accepted() {
        let cfg = make(
            r#"url = "postgres://admin:quest@localhost:8812/qdb"
               connect-timeout = "3s"
               idle-timeout = "60s"
               max-connections = 4
               min-connections = 0
            "#,
        );
        let parsed = QuestDbSinkConfig::try_from(&cfg).expect("ok");
        assert_eq!(parsed.max_connections, Some(4));
    }

    #[test]
    fn rejects_missing_url_prefix() {
        let cfg = make(r#"url = "mysql://nope""#);
        let err = QuestDbSinkConfig::try_from(&cfg).expect_err("bad scheme");
        match err {
            ConfigError::Invalid { reason } => assert!(reason.contains("url")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_field() {
        // `acquire-timeout` is not exposed by this sink; deny_unknown_fields
        // must surface a clear parse error.
        let cfg = make(
            r#"url = "postgres://localhost:8812/qdb"
               acquire-timeout = "5s"
            "#,
        );
        let err = QuestDbSinkConfig::try_from(&cfg).expect_err("unknown field");
        match err {
            ConfigError::TomlParse { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }
}
