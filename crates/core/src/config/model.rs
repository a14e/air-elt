use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Root configuration file (TOML).
///
/// Top-level shape intentionally tracks the README: `[[sources]]`,
/// `[[sinks]]`, `[[storages]]`, `[flow.<name>]`, plus `[secrets]` (string
/// literals used by the `${VAR}` expander). `flow` is a map so cross-file
/// duplicate detection is centralised.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct RootConfig {
    #[serde(default)]
    pub config: IncludesSection,

    #[serde(default)]
    pub secrets: BTreeMap<String, String>,

    #[serde(default)]
    pub sources: Vec<ComponentConfig>,

    #[serde(default)]
    pub sinks: Vec<ComponentConfig>,

    #[serde(default)]
    pub storages: Vec<ComponentConfig>,

    #[serde(default)]
    pub flow: BTreeMap<String, FlowConfig>,
}

#[derive(Debug, Default, Clone, Deserialize, Serialize)]
pub struct IncludesSection {
    #[serde(default)]
    pub include: Vec<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentConfig {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub config: toml::Table,
}

/// Reference to a configured `[[sources]]` instance from a flow block.
///
/// Two surface forms (deserialised via `#[serde(untagged)]`):
///
/// 1. Bare name: `source = "mymongo"` — equivalent to no per-flow
///    options. Backwards-compatible with every existing flow.
/// 2. Developed form: `source = { name = "mymongo", mode = "lookup-on-update" }`
///    — extra keys flow into a free-form `toml::Table` that is
///    pushed into `ReadSpec::source_options` so source connectors
///    can deserialise their own typed shape (mongo-cdc uses this
///    for `mode`). Unknown keys are NOT rejected here because each
///    source validates its own option schema.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum FlowSourceRef {
    Bare(String),
    Detailed {
        name: String,
        #[serde(flatten)]
        options: toml::Table,
    },
}

impl FlowSourceRef {
    pub fn name(&self) -> &str {
        match self {
            FlowSourceRef::Bare(s) => s.as_str(),
            FlowSourceRef::Detailed { name, .. } => name.as_str(),
        }
    }

    pub fn options(&self) -> toml::Table {
        match self {
            FlowSourceRef::Bare(_) => toml::Table::new(),
            FlowSourceRef::Detailed { options, .. } => options.clone(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FlowConfig {
    pub source: FlowSourceRef,
    pub sink: String,
    pub storage: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub mapping: Vec<MappingEntry>,
    /// Cursor config. Pull-based sources (postgres, mysql, mongodb)
    /// require non-empty `fields`. CDC sources (mongo-cdc) require
    /// empty `fields` — pagination is driven by the resume token.
    /// `interval` is meaningful for both: it caps poll cadence /
    /// `maxAwaitTime`. The kind-aware check lives in
    /// `validation::pipeline::assemble`.
    pub cursor: CursorConfig,
    #[serde(default = "default_batch_limit")]
    pub batch_limit: usize,
    #[serde(
        default,
        deserialize_with = "crate::config::interval::deserialize_opt",
        serialize_with = "crate::config::interval::serialize_opt"
    )]
    pub query_timeout: Option<Duration>,

    #[serde(default)]
    pub validation: crate::config::validation::ValidationConfig,

    /// Optional conflict resolution. When absent the sink performs
    /// plain inserts; when set, the sink upserts on `conflict.key`
    /// using the chosen `strategy`.
    #[serde(default)]
    pub conflict: Option<crate::config::conflict::ConflictConfig>,
}

fn default_batch_limit() -> usize {
    1024
}

/// One mapping rule. Single flat shape:
/// `{ from = "a", to = "b", truncate = bool, default = <value> }`.
///
/// `truncate` opts into otherwise-rejected narrowing conversions (text/bytes
/// shrink, integer/float saturate, decimal scale drop, json→text serialize).
/// `default` substitutes when the source value is `Null`. The default
/// literal is parsed against the resolved sink `DataType` at validation
/// time. Bytes columns require a typed prefix (`hex:`, `base64:`, `utf8:`,
/// `bin:`); other types use the plain TOML literal.
///
/// `#[serde(deny_unknown_fields)]` blocks the previously-reserved fields
/// (`transform`, `timezone`, `data-type`) and any future leakage of
/// "for-the-future" knobs into the config surface.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MappingEntry {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub truncate: bool,
    #[serde(default)]
    pub default: Option<toml::Value>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CursorConfig {
    /// Cursor field names. Pull-based sources (postgres/mysql/mongodb)
    /// require this non-empty (validated in `pipeline::assemble`).
    /// CDC sources (mongo-cdc) require it empty.
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default = "default_order")]
    pub order: CursorOrder,
    #[serde(
        default = "default_interval",
        deserialize_with = "crate::config::interval::deserialize",
        serialize_with = "crate::config::interval::serialize"
    )]
    pub interval: std::time::Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorOrder {
    Asc,
    Desc,
}

fn default_order() -> CursorOrder {
    CursorOrder::Asc
}

fn default_interval() -> std::time::Duration {
    std::time::Duration::from_secs(1)
}
