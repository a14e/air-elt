use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_commons::interval;
use air_elt_commons_pg::Dialect;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct PgSourceConfig {
    /// Set by the factory (`postgres` vs `cockroachdb`). Not user-configurable
    /// from TOML — `#[serde(skip)]` keeps the field out of the input surface.
    #[serde(skip)]
    pub dialect: Dialect,
    pub url: String,
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
    pub acquire_timeout: Option<Duration>,
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
    pub max_lifetime: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub statement_timeout: Option<Duration>,
    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub min_connections: Option<u32>,
}

// Why: per-connector TryFrom keeps parsing co-located; see PgStorageConfig.
impl TryFrom<&ComponentConfig> for PgSourceConfig {
    type Error = ConfigError;

    fn try_from(cfg: &ComponentConfig) -> Result<Self, Self::Error> {
        cfg.config
            .clone()
            .try_into::<Self>()
            .map_err(|source| ConfigError::TomlParse {
                path: std::path::PathBuf::from(format!("<inline:{}>", cfg.name)),
                source,
            })
    }
}
