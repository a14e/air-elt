use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::types::data_type::DataType;

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

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct FlowConfig {
    pub source: String,
    pub sink: String,
    pub storage: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub mapping: Vec<MappingEntry>,
    pub cursor: CursorConfig,
    #[serde(default = "default_batch_limit")]
    pub batch_limit: usize,
    #[serde(
        default,
        deserialize_with = "crate::config::interval::deserialize_opt",
        serialize_with = "crate::config::interval::serialize_opt"
    )]
    pub query_timeout: Option<Duration>,
}

fn default_batch_limit() -> usize {
    1024
}

/// One mapping rule. MVP: string form only.
///
/// The object form with `transform`/`timezone`/`data_type` is parsed but the
/// loader emits `ConfigError::UnsupportedInMvp` when any of those are set.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum MappingEntry {
    Simple(SimpleMapping),
    Object(ObjectMapping),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SimpleMapping {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ObjectMapping {
    pub from: ObjectMappingFrom,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct ObjectMappingFrom {
    pub name: String,
    #[serde(default)]
    pub transform: Option<String>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub data_type: Option<DataType>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CursorConfig {
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
