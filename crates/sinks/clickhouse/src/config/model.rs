use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_core::config::interval;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

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

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ChSinkConfig {
    /// HTTP endpoint URL, e.g. `http://localhost:8123`.
    pub url: String,
    /// Default database name. Used as `X-ClickHouse-Database` and
    /// when a flow's `to` is given as a bare table name.
    pub database: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub password: Option<String>,
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
    pub request_timeout: Option<Duration>,
    #[serde(default)]
    pub max_connections: Option<u32>,
}

impl TryFrom<&ComponentConfig> for ChSinkConfig {
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
