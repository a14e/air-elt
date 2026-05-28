//! MongoDB CDC source connector config.
//!
//! Two scopes:
//!
//! * `MongoCdcSourceConfig` — the per-instance `[[sources]] config = { … }`
//!   block. Connection-level: URL, database, pool/timeouts, sample size.
//! * `MongoCdcFlowOptions` — the developed `flow.<x>.source = { name = "...",
//!   mode = "..." }` per-flow opts. Extracted from `ReadSpec.source_options`
//!   inside the source's `build_context`. `mode` is required.
//!
//! Per-flow config lives at the *flow* layer because the same source
//! pool can drive multiple collections that may differ in whether
//! `changeStreamPreAndPostImages` is enabled — each flow chooses its
//! own `mode`.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use air_elt_commons::interval;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MongoCdcSourceConfig {
    pub url: String,
    /// Optional explicit database. When absent we try the trailing
    /// path segment of `url` (e.g. `mongodb://h/appdb`).
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
    /// Per-operation cap (`maxTimeMS`) on driver calls that support it
    /// (find, aggregate). The watch cursor uses `max_await_time` instead.
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub operation_timeout: Option<Duration>,
    /// Long-poll cap on a single `change_stream.next()` await.
    /// Defaults to 1s. Tune up to widen idle blocking, down to lower
    /// per-tick latency at the cost of more wake-ups.
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub max_await_time: Option<Duration>,
    /// Sample size for `describe_schema` (independent from
    /// `validation.sampling.size`). Default 100, capped at
    /// `MAX_SCHEMA_SAMPLE_SIZE`.
    #[serde(default)]
    pub schema_sample_size: Option<usize>,
}

pub const MAX_SCHEMA_SAMPLE_SIZE: usize = 10_000;

impl TryFrom<&ComponentConfig> for MongoCdcSourceConfig {
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
                    "mongo-cdc source {:?}: schema-sample-size {} exceeds cap {}",
                    cfg.name, n, MAX_SCHEMA_SAMPLE_SIZE
                ),
            });
        }
        Ok(parsed)
    }
}

/// Per-flow source options. Required `mode`; deny unknown so a typo
/// doesn't silently switch back to the default.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MongoCdcFlowOptions {
    pub mode: UpdateMode,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateMode {
    /// Server attaches `fullDocument` (post-image) directly on the
    /// change event. Requires `changeStreamPreAndPostImages` enabled
    /// on the watched collection (Mongo 6+). Atomic, no extra
    /// round-trip per update.
    PostImage,
    /// Change stream is opened without `fullDocument`. After each
    /// batch of update events we issue a single
    /// `find({_id: {$in: ids}})` against the collection to pull the
    /// current document state. Cheaper to operate (no server-side
    /// flag) at the cost of one round-trip per update-heavy batch
    /// and a small risk of seeing post-state-after-our-event for
    /// rapidly-mutating documents.
    LookupOnUpdate,
}
