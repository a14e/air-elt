//! Redis sink configuration.
//!
//! Shape: `config = { url = "redis://...", pool = { ... } }`. The `url`
//! is the redis connection string (`redis://` plaintext, `rediss://`
//! TLS); the optional `pool` table tunes the connection pool (see
//! [`air_elt_commons_redis::RedisPoolConfig`]).
//!
//! `deny_unknown_fields` rejects any key the sink does not understand so
//! operator typos surface as clear parse errors. The per-flow `mode`
//! lives on the flow's developed sink form, NOT here — it is per-flow,
//! not per-connector.

use serde::{Deserialize, Serialize};

use air_elt_commons_redis::RedisPoolConfig;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RedisSinkConfig {
    /// Redis connection URL. Required. `redis://` (plaintext) or
    /// `rediss://` (TLS).
    pub url: String,
    /// Connection-pool tunables. All fields optional; omitting `pool`
    /// yields the pool defaults (10 connections).
    #[serde(default)]
    pub pool: RedisPoolConfig,
}

impl RedisSinkConfig {
    fn validate(&self, name: &str) -> Result<(), ConfigError> {
        if !self.url.starts_with("redis://") && !self.url.starts_with("rediss://") {
            return Err(ConfigError::Invalid {
                reason: format!(
                    "redis sink {name:?}: url must start with `redis://` or `rediss://`"
                ),
            });
        }
        Ok(())
    }
}

impl TryFrom<&ComponentConfig> for RedisSinkConfig {
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
            name: "redis".to_string(),
            kind: "redis".to_string(),
            config: table,
        }
    }

    #[test]
    fn minimal_config_accepted() {
        let cfg = make(r#"url = "redis://localhost:6379""#);
        let parsed = RedisSinkConfig::try_from(&cfg).expect("ok");
        assert!(parsed.url.starts_with("redis://"));
    }

    #[test]
    fn pool_table_accepted() {
        let cfg = make(
            r#"url = "redis://localhost:6379"
               pool = { max-connections = 4, acquire-timeout = "2s" }
            "#,
        );
        let parsed = RedisSinkConfig::try_from(&cfg).expect("ok");
        assert_eq!(parsed.pool.max_connections, Some(4));
        assert_eq!(
            parsed.pool.acquire_timeout,
            Some(std::time::Duration::from_secs(2))
        );
    }

    #[test]
    fn rediss_tls_scheme_accepted() {
        let cfg = make(r#"url = "rediss://localhost:6379""#);
        RedisSinkConfig::try_from(&cfg).expect("tls url ok");
    }

    #[test]
    fn rejects_bad_scheme() {
        let cfg = make(r#"url = "http://localhost:6379""#);
        let err = RedisSinkConfig::try_from(&cfg).expect_err("bad scheme");
        match err {
            ConfigError::Invalid { reason } => assert!(reason.contains("url"), "{reason}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_field() {
        let cfg = make(
            r#"url = "redis://localhost:6379"
               bogus = 1
            "#,
        );
        let err = RedisSinkConfig::try_from(&cfg).expect_err("unknown field");
        assert!(matches!(err, ConfigError::TomlParse { .. }));
    }
}
