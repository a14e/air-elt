use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use serde::de::{Deserializer, Error as DeError, MapAccess, Visitor};
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

    #[serde(default)]
    pub metrics: MetricsSection,
}

/// Optional `[metrics]` section. Today only carries the Prometheus
/// sub-config; future surfaces (OTLP, custom exporters) will land
/// alongside.
#[derive(Debug, Default, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsSection {
    #[serde(default)]
    pub prometheus: Option<air_elt_monitoring::PrometheusConfig>,
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
    /// Inverted mapping: keys are sink column names, values are either a
    /// bare string (interpreted as `from` — `"*"` is special: wildcard
    /// when the key is also `"*"`, otherwise body-pack) or a full
    /// [`MappingEntry`] table without `to`.
    #[serde(default)]
    pub mapping: MappingMap,
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

/// Ordered map of sink-column-name → mapping spec.
///
/// Preserves declaration order so wildcard fan-out and explicit
/// overrides produce a deterministic post-expansion shape. Backed by a
/// `Vec` of pairs rather than `HashMap` so insertion order survives the
/// round-trip; duplicate keys are rejected at deserialisation time.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(transparent)]
pub struct MappingMap(pub Vec<(String, MappingRhs)>);

impl MappingMap {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, MappingRhs)> {
        self.0.iter()
    }
}

impl<'a> IntoIterator for &'a MappingMap {
    type Item = &'a (String, MappingRhs);
    type IntoIter = std::slice::Iter<'a, (String, MappingRhs)>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<'de> Deserialize<'de> for MappingMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(MappingMapVisitor)
    }
}

struct MappingMapVisitor;

impl<'de> Visitor<'de> for MappingMapVisitor {
    type Value = MappingMap;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a [mapping] table keyed by sink column name (the old `mapping = [...]` array form \
             is no longer supported)",
        )
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut out: Vec<(String, MappingRhs)> = Vec::with_capacity(map.size_hint().unwrap_or(0));
        let mut seen: ahash::AHashSet<String> = ahash::AHashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(M::Error::custom(format!(
                    "duplicate mapping key {key:?} — sink column names must be unique"
                )));
            }
            let value: MappingRhs = map.next_value()?;
            out.push((key, value));
        }
        Ok(MappingMap(out))
    }

    fn visit_seq<S>(self, _seq: S) -> Result<Self::Value, S::Error>
    where
        S: serde::de::SeqAccess<'de>,
    {
        Err(S::Error::custom(
            "mapping must now be a table keyed by sink column (e.g. `[mapping]\\nsink_col = \
             \"src_col\"`). The legacy `mapping = [...]` array form was removed — see AIR-70.",
        ))
    }
}

/// Right-hand side of one mapping entry.
///
/// Two surface forms:
///
/// 1. **Short** — a bare string. The bare value `"*"` is special:
///    paired with key `"*"` it triggers wildcard fan-out, and paired
///    with any other key it triggers body-pack into that sink column.
///    Otherwise the string is interpreted as the source column name
///    (`from`). Identity (`field = "field"`) and rename
///    (`dst = "src"`) collapse to this same case.
/// 2. **Full** — the long-form table `{ from, truncate?, default?,
///    switch? }`. The map key carries `to`; specifying `to` inside the
///    table is a `deny_unknown_fields` error.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MappingRhs {
    Short(String),
    Full(MappingEntry),
}

impl<'de> Deserialize<'de> for MappingRhs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(MappingRhsVisitor)
    }
}

struct MappingRhsVisitor;

impl<'de> Visitor<'de> for MappingRhsVisitor {
    type Value = MappingRhs;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "a mapping value: either a source column name (string) or a `{ from = \"...\", ... }` \
             table",
        )
    }

    fn visit_str<E: DeError>(self, v: &str) -> Result<Self::Value, E> {
        Ok(MappingRhs::Short(v.to_string()))
    }

    fn visit_string<E: DeError>(self, v: String) -> Result<Self::Value, E> {
        Ok(MappingRhs::Short(v))
    }

    fn visit_borrowed_str<E: DeError>(self, v: &'de str) -> Result<Self::Value, E> {
        Ok(MappingRhs::Short(v.to_string()))
    }

    fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let entry = MappingEntry::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
        Ok(MappingRhs::Full(entry))
    }
}

/// Long-form mapping entry. The sink column name lives on the
/// containing [`MappingMap`] key; `to` is intentionally absent here and
/// `deny_unknown_fields` rejects attempts to specify it.
///
/// `truncate` opts into otherwise-rejected narrowing conversions (text/bytes
/// shrink, integer/float saturate, decimal scale drop, json→text serialize).
/// `default` substitutes when the source value is `Null` (and serves as
/// the fallback when `switch` produces no match). Bytes columns require
/// a typed prefix (`hex:`, `base64:`, `utf8:`, `bin:`); other types use
/// the plain TOML literal.
///
/// `switch` declares a value-to-value lookup table — see
/// [`SwitchTable`] for details.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MappingEntry {
    pub from: String,
    #[serde(default)]
    pub truncate: bool,
    #[serde(default)]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub switch: Option<SwitchTable>,
}

/// Ordered switch lookup table. Each entry maps a source-side value
/// (canonicalised against the source `DataType` at validation time) to
/// a sink-side value. Order is preserved purely for deterministic
/// errors / debug output; runtime lookup is hash-based.
///
/// Keys are TOML strings — TOML inline-table keys are always strings
/// in serde, so an integer-shaped source column accepts both bare
/// `1 = "..."` and quoted `"1" = "..."` forms, both reaching us as
/// `"1"`. The literal is later parsed against the source column type
/// (Int, Bool, Text, Date, …) and canonicalised to the matching
/// `Value`.
#[derive(Debug, Default, Clone, Serialize)]
#[serde(transparent)]
pub struct SwitchTable(pub Vec<(String, toml::Value)>);

impl SwitchTable {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, toml::Value)> {
        self.0.iter()
    }
}

impl<'de> Deserialize<'de> for SwitchTable {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(SwitchTableVisitor)
    }
}

struct SwitchTableVisitor;

impl<'de> Visitor<'de> for SwitchTableVisitor {
    type Value = SwitchTable;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an inline table of switch key → value pairs")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut out: Vec<(String, toml::Value)> = Vec::with_capacity(map.size_hint().unwrap_or(0));
        let mut seen: ahash::AHashSet<String> = ahash::AHashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                return Err(M::Error::custom(format!(
                    "duplicate switch key {key:?} — switch keys must be unique"
                )));
            }
            let value: toml::Value = map.next_value()?;
            out.push((key, value));
        }
        Ok(SwitchTable(out))
    }
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
    /// Per-flow startup jitter that shifts the first-tick schedule grid
    /// by a deterministic name-hashed offset in `[0, jitter)`. Tames
    /// thundering-herd CPU spikes when many flows share the same
    /// `interval`. When omitted, defaults to `min(interval, 5min)` —
    /// the full interval, hard-capped at five minutes so a 24h cadence
    /// doesn't introduce hours of startup wait. `"0s"` disables jitter
    /// entirely — useful for tests / deterministic e2e runs. Resolved
    /// via [`CursorConfig::effective_jitter`]; validated at config-load
    /// against `jitter <= interval`.
    #[serde(
        default,
        deserialize_with = "crate::config::interval::deserialize_opt_allow_zero",
        serialize_with = "crate::config::interval::serialize_opt"
    )]
    pub jitter: Option<std::time::Duration>,
}

/// Cap for the auto-defaulted jitter: the full `interval` is clipped to
/// this duration so a flow with `interval = 24h` doesn't sleep 24h
/// before its first tick. Explicit operator-set values are not capped
/// (they still pass the `jitter <= interval` rule).
pub const JITTER_DEFAULT_CAP: std::time::Duration = std::time::Duration::from_secs(5 * 60);

impl CursorConfig {
    /// Resolve the effective jitter for this cursor. When `jitter` is
    /// `Some(d)` it's returned as-is (including `"0s"` which disables
    /// jitter); when `None`, defaults to `min(interval, 5min)` — the
    /// full interval, capped at five minutes.
    pub fn effective_jitter(&self) -> std::time::Duration {
        match self.jitter {
            Some(d) => d,
            None => self.interval.min(JITTER_DEFAULT_CAP),
        }
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn cursor(interval: Duration, jitter: Option<Duration>) -> CursorConfig {
        CursorConfig {
            fields: Vec::new(),
            order: CursorOrder::Asc,
            interval,
            jitter,
        }
    }

    /// When `jitter` is absent, the default is `min(interval, 5min)`:
    /// the full interval, clipped so a 24h interval doesn't introduce a
    /// 24h startup wait. Spreading concurrent flows across the full
    /// cadence period maximises fan-in smoothing per backend pool.
    #[test]
    fn effective_jitter_defaults_to_full_interval_capped_at_five_minutes() {
        // 1s → 1s (interval below cap)
        assert_eq!(
            cursor(Duration::from_secs(1), None).effective_jitter(),
            Duration::from_secs(1)
        );
        // 60s → 60s (interval below cap)
        assert_eq!(
            cursor(Duration::from_secs(60), None).effective_jitter(),
            Duration::from_secs(60)
        );
        // 1h → cap (5min)
        assert_eq!(
            cursor(Duration::from_secs(3600), None).effective_jitter(),
            Duration::from_secs(300),
        );
        // 24h → cap
        assert_eq!(
            cursor(Duration::from_secs(86_400), None).effective_jitter(),
            Duration::from_secs(300),
        );
    }

    /// Operator-set `jitter` is passed through verbatim — including
    /// `"0s"`, which disables jitter entirely. The default rule only
    /// fires when the field is `None`.
    #[test]
    fn effective_jitter_explicit_passthrough_including_zero() {
        assert_eq!(
            cursor(Duration::from_secs(60), Some(Duration::from_secs(2))).effective_jitter(),
            Duration::from_secs(2)
        );
        assert_eq!(
            cursor(Duration::from_secs(60), Some(Duration::ZERO)).effective_jitter(),
            Duration::ZERO,
        );
    }
}
