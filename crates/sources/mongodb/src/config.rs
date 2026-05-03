//! MongoDB source connector config (the connector-specific
//! `config = { … }` block under `[[sources]]`).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_core::config::interval;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MongoSourceConfig {
    pub url: String,
    /// Optional explicit database name. When absent we try the
    /// trailing path segment of `url` (e.g. `mongodb://h/appdb`).
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
    #[serde(default)]
    pub max_connections: Option<u32>,
    #[serde(default)]
    pub min_connections: Option<u32>,
    /// Per-operation cap (`maxTimeMS`) applied via the driver's
    /// per-`*Options::max_time` field on every Mongo call. Bounds
    /// server-side work even after the runner detaches a spawned
    /// future on shutdown / timeout (the `mongodb` 3.x driver is not
    /// cancellation-safe — the runner uses `tokio::spawn` + detach
    /// instead of `tokio::time::timeout`). Defaults to
    /// `PoolSettings::defaults().statement` (30s).
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub operation_timeout: Option<Duration>,
    /// Sample size used by `describe_schema` to infer the per-flow
    /// schema. Independent from `validation.sampling.size`. Default
    /// 100, capped at `MAX_SCHEMA_SAMPLE_SIZE`.
    #[serde(default)]
    pub schema_sample_size: Option<usize>,
}

/// Upper bound on `schema_sample_size`. The Mongo driver buffers a
/// cursor over the requested document count and `infer_schema_from_sample`
/// builds an in-memory trie sized by `sample × leaves_per_doc`. A
/// pathologically large value (millions) would OOM the validator. Match
/// the spirit of the loader's existing `batch_limit × mapping_cols ≤
/// 60_000` cap.
pub const MAX_SCHEMA_SAMPLE_SIZE: usize = 10_000;

impl TryFrom<&ComponentConfig> for MongoSourceConfig {
    type Error = ConfigError;

    fn try_from(cfg: &ComponentConfig) -> Result<Self, Self::Error> {
        let parsed: Self =
            cfg.config
                .clone()
                .try_into()
                .map_err(|source| ConfigError::TomlParse {
                    path: std::path::PathBuf::from(format!("<inline:{}>", cfg.name)),
                    source,
                })?;
        if let Some(n) = parsed.schema_sample_size
            && n > MAX_SCHEMA_SAMPLE_SIZE
        {
            return Err(ConfigError::Invalid {
                reason: format!(
                    "mongodb source {:?}: schema-sample-size {} exceeds cap {} — \
                     larger samples would OOM the validator",
                    cfg.name, n, MAX_SCHEMA_SAMPLE_SIZE
                ),
            });
        }
        Ok(parsed)
    }
}
