use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_core::config::interval;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MongoSinkConfig {
    pub url: String,
    #[serde(default)]
    pub database: Option<String>,
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
    /// Per-operation cap applied as `maxTimeMS` on every server-side
    /// write/read. Bounds runaway server work after a runner-side
    /// timeout drops the outer future (the spawned task continues
    /// driving the driver future to completion — `maxTimeMS` is what
    /// stops it on the server). Defaults to `PoolSettings::statement`
    /// (30s).
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub operation_timeout: Option<Duration>,
    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub min_connections: Option<u32>,
}

impl TryFrom<&ComponentConfig> for MongoSinkConfig {
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
