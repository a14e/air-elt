use std::collections::BTreeMap;

use ahash::AHashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::debug;

use crate::config::env_expand;
use crate::config::model::{MappingMap, MappingRhs, RootConfig};
use crate::error::ConfigError;

/// Hard cap on a single config file. Operator-controlled files in MVP are
/// typically a few KB; 16 MiB is comfortably above anything real and low
/// enough that a pathological `include = ["/"]` shouldn't OOM the process.
const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

/// On-disk format of a single config file. Determined by extension —
/// `.toml` → Toml, `.yml`/`.yaml` → Yaml. Anything else is rejected at
/// read time with `ConfigError::UnknownConfigExtension`.
#[derive(Debug, Clone, Copy)]
enum ConfigFormat {
    Toml,
    Yaml,
}

/// Load the root config from `path`, merge any files listed in
/// `[config].include`, and expand `${VAR}` / `${VAR:default}` references.
///
/// Include entries are **relative to the including file's directory**;
/// absolute paths are rejected so operators can't accidentally walk the whole
/// filesystem. Directories are read non-recursively for `*.toml`/`*.yml`/
/// `*.yaml` files in sorted order. Canonical paths are tracked in a AHashSet
/// so symlink loops visit each real file at most once.
///
/// Format dispatch is per-file by extension, so a TOML root can include YAML
/// fragments and vice versa.
pub fn load<P: AsRef<Path>>(path: P) -> Result<RootConfig, ConfigError> {
    let path = path.as_ref().to_path_buf();
    let mut cycle = PathCycleDetector::new();

    let format = detect_format(&path)?;
    let raw_root = read_single(&path)?;
    cycle.mark(&path);
    let mut secrets_origin: BTreeMap<String, PathBuf> = BTreeMap::new();
    let secrets = extract_secrets(&raw_root, &path, format)?;
    for k in secrets.keys() {
        secrets_origin.insert(k.clone(), path.clone());
    }
    let expanded_root = env_expand::expand(&raw_root, &secrets)?;
    let mut root = parse_root(&expanded_root, &path, format)?;

    let base_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let includes = std::mem::take(&mut root.config.include);

    for include in includes {
        if include.is_absolute() {
            return Err(ConfigError::AbsoluteIncludeNotAllowed {
                path: include.display().to_string(),
            });
        }
        let resolved = base_dir.join(include);
        merge_include(
            &mut root,
            &resolved,
            &secrets,
            &mut cycle,
            &mut secrets_origin,
        )?;
    }

    validate_post_merge(&root)?;
    Ok(root)
}

fn detect_format(path: &Path) -> Result<ConfigFormat, ConfigError> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "toml" => Ok(ConfigFormat::Toml),
        "yml" | "yaml" => Ok(ConfigFormat::Yaml),
        _ => Err(ConfigError::UnknownConfigExtension {
            path: path.to_path_buf(),
            ext,
        }),
    }
}

fn read_single(path: &Path) -> Result<String, ConfigError> {
    debug!(?path, "reading config file");
    let metadata = std::fs::metadata(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_CONFIG_BYTES {
        return Err(ConfigError::ConfigTooLarge {
            max: MAX_CONFIG_BYTES,
            actual: metadata.len(),
        });
    }
    std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn parse_root(
    expanded: &str,
    path: &Path,
    format: ConfigFormat,
) -> Result<RootConfig, ConfigError> {
    match format {
        ConfigFormat::Toml => {
            toml::from_str::<RootConfig>(expanded).map_err(|source| ConfigError::TomlParse {
                path: path.to_path_buf(),
                source,
            })
        }
        ConfigFormat::Yaml => parse_yaml_root(expanded, path),
    }
}

/// Parse a YAML payload that may contain multiple `---`-separated documents
/// and merge them into one logical `RootConfig`. Each document is treated
/// like a separate include of the same file: arrays of sources/sinks/storages
/// concatenate, the `flow` map merges, `config.include` concatenates, and
/// duplicate names across documents are rejected with the same errors a
/// cross-file include would produce.
fn parse_yaml_root(expanded: &str, path: &Path) -> Result<RootConfig, ConfigError> {
    let mut merged = RootConfig::default();
    let mut secrets_origin: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut saw_doc = false;
    for doc in serde_yaml::Deserializer::from_str(expanded) {
        let extra = RootConfig::deserialize(doc).map_err(|source| ConfigError::YamlParse {
            path: path.to_path_buf(),
            source,
        })?;
        saw_doc = true;
        merge_extra_into(&mut merged, extra, path, &mut secrets_origin)?;
    }
    if !saw_doc {
        // `serde_yaml::Deserializer` yields zero documents on empty input;
        // preserve the historical behaviour (empty file → default RootConfig).
        return Ok(RootConfig::default());
    }
    Ok(merged)
}

/// A minimal first-pass parse that only extracts `[secrets]`. We need secrets
/// before we expand the rest of the file, but the rest of the file can contain
/// `${VAR}` references in otherwise-typed positions (numbers, enums) which
/// would fail the stricter final parse — so we explicitly opt into a
/// permissive shape here.
fn extract_secrets(
    raw: &str,
    path: &Path,
    format: ConfigFormat,
) -> Result<BTreeMap<String, String>, ConfigError> {
    #[derive(Deserialize, Default)]
    struct SecretsOnly {
        #[serde(default)]
        secrets: BTreeMap<String, String>,
    }
    match format {
        ConfigFormat::Toml => {
            let parsed: SecretsOnly =
                toml::from_str(raw).map_err(|source| ConfigError::TomlParse {
                    path: path.to_path_buf(),
                    source,
                })?;
            Ok(parsed.secrets)
        }
        ConfigFormat::Yaml => {
            // Multi-doc YAML: each `---` section may declare its own
            // `secrets`. Mirror the cross-file rule — a key declared in two
            // documents of the same file is a `DuplicateSecret` error
            // against the same path twice.
            let mut merged: BTreeMap<String, String> = BTreeMap::new();
            for doc in serde_yaml::Deserializer::from_str(raw) {
                let parsed =
                    SecretsOnly::deserialize(doc).map_err(|source| ConfigError::YamlParse {
                        path: path.to_path_buf(),
                        source,
                    })?;
                for (k, v) in parsed.secrets {
                    if merged.contains_key(&k) {
                        return Err(ConfigError::DuplicateSecret {
                            key: k,
                            first: path.to_path_buf(),
                            second: path.to_path_buf(),
                        });
                    }
                    merged.insert(k, v);
                }
            }
            Ok(merged)
        }
    }
}

struct PathCycleDetector {
    visited: AHashSet<PathBuf>,
}

impl PathCycleDetector {
    fn new() -> Self {
        Self {
            visited: AHashSet::new(),
        }
    }

    fn mark(&mut self, path: &Path) {
        if let Ok(canon) = std::fs::canonicalize(path) {
            self.visited.insert(canon);
        }
    }

    fn seen(&self, path: &Path) -> bool {
        std::fs::canonicalize(path)
            .map(|canon| self.visited.contains(&canon))
            .unwrap_or(false)
    }
}

fn merge_include(
    root: &mut RootConfig,
    include: &Path,
    secrets: &BTreeMap<String, String>,
    cycle: &mut PathCycleDetector,
    secrets_origin: &mut BTreeMap<String, PathBuf>,
) -> Result<(), ConfigError> {
    if include.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(include)
            .map_err(|source| ConfigError::Io {
                path: include.to_path_buf(),
                source,
            })?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|s| s.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .is_some_and(|e| matches!(e.as_str(), "toml" | "yml" | "yaml"))
            })
            .collect();
        entries.sort();
        for entry in entries {
            if cycle.seen(&entry) {
                debug!(path = ?entry, "skipping already-visited include file");
                continue;
            }
            merge_file(root, &entry, secrets, cycle, secrets_origin)?;
        }
        Ok(())
    } else {
        if cycle.seen(include) {
            debug!(path = ?include, "skipping already-visited include file");
            return Ok(());
        }
        merge_file(root, include, secrets, cycle, secrets_origin)
    }
}

fn merge_file(
    root: &mut RootConfig,
    path: &Path,
    secrets: &BTreeMap<String, String>,
    cycle: &mut PathCycleDetector,
    secrets_origin: &mut BTreeMap<String, PathBuf>,
) -> Result<(), ConfigError> {
    cycle.mark(path);
    let format = detect_format(path)?;
    let raw = read_single(path)?;
    let expanded = env_expand::expand(&raw, secrets)?;
    let extra = parse_root(&expanded, path, format)?;
    merge_extra_into(root, extra, path, secrets_origin)
}

/// Fold `extra` into `root` using the project's "one definition per name,
/// anywhere" rule for sources/sinks/storages/flow/secrets. Used both when
/// merging an `include`'d file and when stitching together `---`-separated
/// YAML documents inside a single file.
fn merge_extra_into(
    root: &mut RootConfig,
    extra: RootConfig,
    path: &Path,
    secrets_origin: &mut BTreeMap<String, PathBuf>,
) -> Result<(), ConfigError> {
    for inc in extra.config.include {
        root.config.include.push(inc);
    }
    for (name, flow) in extra.flow {
        if root.flow.contains_key(&name) {
            return Err(ConfigError::DuplicateFlow { name });
        }
        root.flow.insert(name, flow);
    }
    for src in extra.sources {
        if root.sources.iter().any(|s| s.name == src.name) {
            return Err(ConfigError::DuplicateName {
                kind: "source",
                name: src.name,
            });
        }
        root.sources.push(src);
    }
    for sink in extra.sinks {
        if root.sinks.iter().any(|s| s.name == sink.name) {
            return Err(ConfigError::DuplicateName {
                kind: "sink",
                name: sink.name,
            });
        }
        root.sinks.push(sink);
    }
    for st in extra.storages {
        if root.storages.iter().any(|s| s.name == st.name) {
            return Err(ConfigError::DuplicateName {
                kind: "storage",
                name: st.name,
            });
        }
        root.storages.push(st);
    }
    // Secrets must be declared exactly once across the include graph.
    // Mirroring sources/sinks/storages/flow keeps the rule single-shape:
    // "one definition per name, anywhere". Silent first-wins behaviour was
    // dropped because it made operator intent depend on filesystem ordering.
    for (k, v) in extra.secrets {
        if let Some(prev_path) = secrets_origin.get(&k) {
            return Err(ConfigError::DuplicateSecret {
                key: k,
                first: prev_path.clone(),
                second: path.to_path_buf(),
            });
        }
        secrets_origin.insert(k.clone(), path.to_path_buf());
        root.secrets.insert(k, v);
    }
    Ok(())
}

/// `true` iff `mapping` contains any entry that triggers post-expansion
/// resolution (wildcard `"*" = "*"` or body-pack `NAME = "*"`). Such
/// entries invalidate the loader-time subset checks
/// (`cursor.fields ⊆ mapping.from`, `conflict.key ⊆ mapping.to`,
/// `batch_limit × cols ≤ 60_000`) because the actual column set is
/// unknown until expansion runs in `validation::pipeline::validate`.
fn mapping_defers_subset_checks(mapping: &MappingMap) -> bool {
    mapping.iter().any(|(_, rhs)| match rhs {
        MappingRhs::Short(s) => s == "*",
        MappingRhs::Full(entry) => entry.from == "*",
    })
}

/// Pull `(from, to)` out of a mapping entry when it is statically
/// determinable at loader time. Returns `None` for entries whose RHS is
/// the wildcard marker `"*"` (handled by `mapping::expand` later — the
/// caller must have already decided not to perform subset checks). The
/// `to` side is always the map key.
fn extract_static_pair(entry: &(String, MappingRhs)) -> Option<(&str, &str)> {
    let (to, rhs) = entry;
    match rhs {
        MappingRhs::Full(e) => {
            if e.from == "*" {
                None
            } else {
                Some((e.from.as_str(), to.as_str()))
            }
        }
        MappingRhs::Short(s) => {
            if s == "*" {
                None
            } else {
                Some((s.as_str(), to.as_str()))
            }
        }
    }
}

/// Structural checks after all files are merged.
fn validate_post_merge(root: &RootConfig) -> Result<(), ConfigError> {
    for (flow_name, flow) in &root.flow {
        // Per-source-kind cursor.fields shape (non-empty for pull-based,
        // empty for cdc) is checked in `validation::pipeline::assemble`
        // where the source kind is known. The loader only ensures
        // structural sanity (mapping subset, interval > 0, …).
        if flow.batch_limit == 0 {
            return Err(ConfigError::Invalid {
                reason: format!("flow {flow_name:?} has batch-limit = 0"),
            });
        }
        if flow.cursor.interval.is_zero() {
            return Err(ConfigError::Invalid {
                reason: format!("flow {flow_name:?} has zero cursor interval"),
            });
        }
        if let Some(t) = flow.query_timeout {
            if t.is_zero() {
                return Err(ConfigError::Invalid {
                    reason: format!("flow {flow_name:?} has zero query-timeout"),
                });
            }
        }
        // When mapping contains a wildcard or JSON auto-pack rule, the
        // post-expansion column set isn't known yet — defer the bind
        // and subset checks to `validation::pipeline::validate`, which
        // re-runs them after `mapping::expand` produces the concrete
        // expanded column list.
        let defer = mapping_defers_subset_checks(&flow.mapping);

        if !defer {
            // Why: Postgres rejects statements with more than 65 535 bind
            // parameters (wire protocol uses u16 for bind-count). A sink
            // batch of N rows over C mapped columns emits N*C binds; we
            // cap below the hard limit so operators see a clear error at
            // validate rather than sqlx complaining mid-drain. Source
            // SELECTs only bind cursor fields per batch, so the check is
            // guided by sink shape.
            let cols = flow.mapping.len();
            if flow.batch_limit.saturating_mul(cols) > 60_000 {
                return Err(ConfigError::Invalid {
                    reason: format!(
                        "flow {flow_name:?}: batch_limit={} × mapping_cols={} exceeds 60_000 bind parameters",
                        flow.batch_limit, cols
                    ),
                });
            }
            // Cursor fields must appear in mapping.from — otherwise the
            // source SELECT will not project them and runtime will emit
            // a misleading error.
            let mapped_froms: AHashSet<&str> = flow
                .mapping
                .iter()
                .filter_map(extract_static_pair)
                .map(|(from, _)| from)
                .collect();
            for cf in &flow.cursor.fields {
                if !mapped_froms.contains(cf.as_str()) {
                    return Err(ConfigError::Invalid {
                        reason: format!(
                            "flow {flow_name:?}: cursor field {cf:?} must be listed in mapping.from"
                        ),
                    });
                }
            }
        }
        if let Some(conflict) = &flow.conflict {
            conflict.validate().map_err(|reason| ConfigError::Invalid {
                reason: format!("flow {flow_name:?}: {reason}"),
            })?;
            if !defer {
                // Every conflict.key entry must appear in mapping.to —
                // otherwise the upsert filter would reference a sink
                // column we never write.
                let mapped_tos: AHashSet<&str> = flow
                    .mapping
                    .iter()
                    .filter_map(extract_static_pair)
                    .map(|(_, to)| to)
                    .collect();
                for k in &conflict.key {
                    if !mapped_tos.contains(k.as_str()) {
                        return Err(ConfigError::Invalid {
                            reason: format!(
                                "flow {flow_name:?}: conflict.key entry {k:?} must be listed in mapping.to"
                            ),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn load_single_file_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg_src"
type = "postgres"
config = { url = "postgres://x" }

[[sinks]]
name = "pg_sink"
type = "postgres"
config = { url = "postgres://y" }

[[storages]]
name = "pg_state"
type = "postgres"
config = { url = "postgres://z" }

[flow.users]
source = "pg_src"
sink = "pg_sink"
storage = "pg_state"
from = "public.users"
to = "analytics.users"
cursor = { fields = ["id"], order = "asc", interval = "1s" }
[flow.users.mapping]
id = "id"
name = "name"
"#,
        );
        let root = load(&path).unwrap();
        assert_eq!(root.sources.len(), 1);
        assert_eq!(root.flow.len(), 1);
        assert_eq!(root.flow["users"].from, "public.users");
    }

    #[test]
    fn expands_env_reference() {
        // Why: isolated env var name and a local tempdir → safe even under
        // parallel cargo-test execution.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("AIR_ELT_TEST_LOADER_URL", "postgres://expanded");
        }
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "s"
type = "postgres"
config = { url = "${AIR_ELT_TEST_LOADER_URL}" }

[[sinks]]
name = "s"
type = "postgres"
config = { url = "postgres://k" }

[[storages]]
name = "s"
type = "postgres"
config = { url = "postgres://z" }

[flow.f]
source = "s"
sink = "s"
storage = "s"
from = "t"
to = "t"
cursor = { fields = ["id"] }
[flow.f.mapping]
id = "id"
"#,
        );
        let root = load(&path).unwrap();
        assert_eq!(
            root.sources[0].config.get("url").unwrap().as_str(),
            Some("postgres://expanded")
        );
    }

    /// `transform` / `timezone` / `data-type` were placeholder reserved
    /// fields. The "no future-proofing config fields" rule means they no
    /// longer exist in the model — `deny_unknown_fields` makes them parse
    /// errors, which is a stronger guarantee than the previous
    /// `UnsupportedInMvp` runtime check.
    #[test]
    fn reserved_mapping_fields_now_parse_error() {
        let dir = tempfile::tempdir().unwrap();
        for field in [
            "transform = \"seconds\"",
            "timezone = \"UTC\"",
            "data-type = \"text\"",
        ] {
            let mapping_line =
                format!("mapping = [{{ from = \"created_at\", to = \"x\", {field} }}]");
            let cfg = format!(
                r#"
[[sources]]
name = "pg"
type = "postgres"
config = {{}}
[[sinks]]
name = "pg"
type = "postgres"
config = {{}}
[[storages]]
name = "pg"
type = "postgres"
config = {{}}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
{mapping_line}
cursor = {{ fields = ["created_at"] }}
"#
            );
            let path = write(dir.path(), "config.toml", &cfg);
            let err = load(&path).unwrap_err();
            assert!(
                matches!(err, ConfigError::TomlParse { .. }),
                "expected TomlParse for reserved field {field:?}, got {err:?}"
            );
        }
    }

    #[test]
    fn cursor_must_be_in_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
cursor = { fields = ["created_at"] }
[flow.f.mapping]
id = "id"
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn bind_param_limit_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut mapping_lines = String::new();
        for i in 0..20 {
            mapping_lines.push_str(&format!("c{i} = \"c{i}\"\n"));
        }
        let path = write(
            dir.path(),
            "config.toml",
            &format!(
                r#"
[[sources]]
name = "pg"
type = "postgres"
config = {{}}
[[sinks]]
name = "pg"
type = "postgres"
config = {{}}
[[storages]]
name = "pg"
type = "postgres"
config = {{}}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
batch-limit = 5000
cursor = {{ fields = ["c0"] }}
[flow.f.mapping]
{mapping_lines}
"#
            ),
        );
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn zero_cursor_interval_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
cursor = { fields = ["id"], interval = "0s" }
[flow.f.mapping]
id = "id"
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(
            err.to_string().contains("zero"),
            "expected zero-duration error, got: {err}"
        );
    }

    #[test]
    fn zero_query_timeout_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
query-timeout = "0s"
cursor = { fields = ["id"] }
[flow.f.mapping]
id = "id"
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(
            err.to_string().contains("zero"),
            "expected zero-duration error, got: {err}"
        );
    }

    #[test]
    fn absolute_include_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["/etc"]

[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::AbsoluteIncludeNotAllowed { .. }));
    }

    #[test]
    fn load_single_file_yaml_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - name: pg_src
    type: postgres
    config:
      url: "postgres://x"

sinks:
  - name: pg_sink
    type: postgres
    config:
      url: "postgres://y"

storages:
  - name: pg_state
    type: postgres
    config:
      url: "postgres://z"

flow:
  users:
    source: pg_src
    sink: pg_sink
    storage: pg_state
    from: public.users
    to: analytics.users
    mapping:
      id: id
      name: name
    cursor:
      fields: [id]
      order: asc
      interval: "1s"
"#,
        );
        let root = load(&path).unwrap();
        assert_eq!(root.sources.len(), 1);
        assert_eq!(root.flow.len(), 1);
        assert_eq!(root.flow["users"].from, "public.users");
    }

    /// Locks the YAML→`toml::Table` round-trip for `[[sources]].config`.
    /// `ComponentConfig.config` is a `toml::Table`, so the loader implicitly
    /// requires every YAML scalar/map shape we accept to be representable in
    /// `toml::Value`. This test feeds non-string scalars (integer, duration
    /// string, nested map) to surface any format coupling that could regress.
    #[test]
    fn yaml_config_with_non_string_scalars_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - name: pg
    type: postgres
    config:
      url: "postgres://x"
      max-connections: 10
      connect-timeout: "5s"
      nested:
        key: "value"
        flag: true
sinks:
  - { name: pg, type: postgres, config: {} }
storages:
  - { name: pg, type: postgres, config: {} }
flow:
  f:
    source: pg
    sink: pg
    storage: pg
    from: t
    to: t
    mapping: { id: id }
    cursor: { fields: [id] }
"#,
        );
        let root = load(&path).unwrap();
        let cfg = &root.sources[0].config;
        assert_eq!(
            cfg.get("max-connections").and_then(|v| v.as_integer()),
            Some(10)
        );
        assert_eq!(
            cfg.get("connect-timeout").and_then(|v| v.as_str()),
            Some("5s")
        );
        let nested = cfg.get("nested").and_then(|v| v.as_table()).unwrap();
        assert_eq!(nested.get("flag").and_then(|v| v.as_bool()), Some(true));
    }

    #[test]
    fn toml_root_includes_yaml_fragment() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["flows"]

[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        std::fs::create_dir(dir.path().join("flows")).unwrap();
        write(
            &dir.path().join("flows"),
            "users.yml",
            r#"
flow:
  users:
    source: pg
    sink: pg
    storage: pg
    from: users
    to: users
    mapping:
      id: id
    cursor: { fields: [id] }
"#,
        );
        let root = load(dir.path().join("config.toml")).unwrap();
        assert!(root.flow.contains_key("users"));
    }

    #[test]
    fn yaml_root_includes_toml_fragment() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.yml",
            r#"
config:
  include: [flows]
sources:
  - { name: pg, type: postgres, config: {} }
sinks:
  - { name: pg, type: postgres, config: {} }
storages:
  - { name: pg, type: postgres, config: {} }
"#,
        );
        std::fs::create_dir(dir.path().join("flows")).unwrap();
        write(
            &dir.path().join("flows"),
            "users.toml",
            r#"
[flow.users]
source = "pg"
sink = "pg"
storage = "pg"
from = "users"
to = "users"
cursor = { fields = ["id"] }
[flow.users.mapping]
id = "id"
"#,
        );
        let root = load(dir.path().join("config.yml")).unwrap();
        assert!(root.flow.contains_key("users"));
    }

    #[test]
    fn dir_scan_picks_up_all_three_extensions() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["flows"]
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        std::fs::create_dir(dir.path().join("flows")).unwrap();
        write(
            &dir.path().join("flows"),
            "a.toml",
            r#"
[flow.a]
source = "pg"
sink = "pg"
storage = "pg"
from = "a"
to = "a"
cursor = { fields = ["id"] }
[flow.a.mapping]
id = "id"
"#,
        );
        write(
            &dir.path().join("flows"),
            "b.yml",
            r#"
flow:
  b:
    source: pg
    sink: pg
    storage: pg
    from: b
    to: b
    mapping: { id: id }
    cursor: { fields: [id] }
"#,
        );
        write(
            &dir.path().join("flows"),
            "c.yaml",
            r#"
flow:
  c:
    source: pg
    sink: pg
    storage: pg
    from: c
    to: c
    mapping: { id: id }
    cursor: { fields: [id] }
"#,
        );
        let root = load(dir.path().join("config.toml")).unwrap();
        assert!(root.flow.contains_key("a"));
        assert!(root.flow.contains_key("b"));
        assert!(root.flow.contains_key("c"));
    }

    #[test]
    fn duplicate_flow_across_formats_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["flows"]
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        std::fs::create_dir(dir.path().join("flows")).unwrap();
        write(
            &dir.path().join("flows"),
            "a.toml",
            r#"
[flow.users]
source = "pg"
sink = "pg"
storage = "pg"
from = "u"
to = "u"
cursor = { fields = ["id"] }
[flow.users.mapping]
id = "id"
"#,
        );
        write(
            &dir.path().join("flows"),
            "b.yml",
            r#"
flow:
  users:
    source: pg
    sink: pg
    storage: pg
    from: u
    to: u
    mapping: { id: id }
    cursor: { fields: [id] }
"#,
        );
        let err = load(dir.path().join("config.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateFlow { .. }));
    }

    #[test]
    fn duplicate_secret_across_files_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["extras.yml"]

[secrets]
TOKEN = "first"

[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        write(
            dir.path(),
            "extras.yml",
            r#"
secrets:
  TOKEN: "second"
"#,
        );
        let err = load(dir.path().join("config.toml")).unwrap_err();
        match err {
            ConfigError::DuplicateSecret {
                ref key,
                ref first,
                ref second,
            } => {
                assert_eq!(key, "TOKEN");
                assert!(
                    first.ends_with("config.toml"),
                    "first path should be the root, got {first:?}"
                );
                assert!(
                    second.ends_with("extras.yml"),
                    "second path should be the include, got {second:?}"
                );
            }
            other => panic!("expected DuplicateSecret, got {other:?}"),
        }
    }

    /// Two siblings declaring the same secret must collide just like a
    /// root↔child collision — the rule is "exactly one definition across the
    /// whole include graph", regardless of how the graph is wired.
    #[test]
    fn duplicate_secret_between_sibling_includes_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["bits"]

[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        std::fs::create_dir(dir.path().join("bits")).unwrap();
        write(
            &dir.path().join("bits"),
            "a.toml",
            r#"
[secrets]
TOKEN = "first"
"#,
        );
        write(
            &dir.path().join("bits"),
            "b.yml",
            r#"
secrets:
  TOKEN: "second"
"#,
        );
        let err = load(dir.path().join("config.toml")).unwrap_err();
        match err {
            ConfigError::DuplicateSecret {
                ref key,
                ref first,
                ref second,
            } => {
                assert_eq!(key, "TOKEN");
                // Dir scan is alphabetical: a.toml is `first`, b.yml is `second`.
                assert!(first.ends_with("a.toml"), "got first={first:?}");
                assert!(second.ends_with("b.yml"), "got second={second:?}");
            }
            other => panic!("expected DuplicateSecret, got {other:?}"),
        }
    }

    #[test]
    fn unknown_extension_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        // Why a TOML-shaped body: detect_format runs before any parse, so
        // even content that *would* parse as valid TOML must be rejected
        // purely on the extension. Empty bodies pass trivially and don't
        // discriminate between extension- vs content-driven rejection.
        let path = write(dir.path(), "config.json", "name = \"x\"\n");
        let err = load(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::UnknownConfigExtension { ref ext, .. } if ext == "json"),
            "expected UnknownConfigExtension(json), got {err:?}"
        );
    }

    #[test]
    fn broken_toml_reports_toml_parse() {
        let dir = tempfile::tempdir().unwrap();
        // Unterminated array-of-tables header — hard syntax error in TOML.
        let path = write(dir.path(), "config.toml", "[[sources\n");
        let err = load(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::TomlParse { .. }),
            "expected TomlParse, got {err:?}"
        );
    }

    #[test]
    fn broken_yaml_reports_yaml_parse() {
        let dir = tempfile::tempdir().unwrap();
        // Why: an unclosed flow-mapping is a hard YAML syntax error, not a
        // shape error — guarantees we hit serde_yaml's parser branch and
        // surface it as `YamlParse`, not `Invalid`.
        let path = write(dir.path(), "config.yml", "sources: [{name: pg, type:\n");
        let err = load(&path).unwrap_err();
        assert!(
            matches!(err, ConfigError::YamlParse { .. }),
            "expected YamlParse, got {err:?}"
        );
    }

    #[test]
    fn includes_are_merged_and_duplicates_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "config.toml",
            r#"
[config]
include = ["flows"]

[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
"#,
        );
        std::fs::create_dir(dir.path().join("flows")).unwrap();
        write(
            &dir.path().join("flows"),
            "users.toml",
            r#"
[flow.users]
source = "pg"
sink = "pg"
storage = "pg"
from = "users"
to = "users"
cursor = { fields = ["id"] }
[flow.users.mapping]
id = "id"
"#,
        );
        write(
            &dir.path().join("flows"),
            "dup.toml",
            r#"
[flow.users]
source = "pg"
sink = "pg"
storage = "pg"
from = "users"
to = "users"
cursor = { fields = ["id"] }
[flow.users.mapping]
id = "id"
"#,
        );
        let err = load(dir.path().join("config.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateFlow { .. }));
    }

    /// `---`-separated YAML documents in one file are folded into one
    /// logical config: source/sink/storage arrays concatenate, the flow
    /// map merges. Lets operators split a single file along role lines
    /// without inventing per-role files.
    #[test]
    fn yaml_multi_document_root_merges_arrays_and_flow() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - { name: pg_src, type: postgres, config: { url: "postgres://x" } }
---
sinks:
  - { name: pg_sink, type: postgres, config: { url: "postgres://y" } }
---
storages:
  - { name: pg_state, type: postgres, config: { url: "postgres://z" } }
flow:
  users:
    source: pg_src
    sink: pg_sink
    storage: pg_state
    from: users
    to: users
    mapping: { id: id }
    cursor: { fields: [id] }
"#,
        );
        let root = load(&path).unwrap();
        assert_eq!(root.sources.len(), 1);
        assert_eq!(root.sinks.len(), 1);
        assert_eq!(root.storages.len(), 1);
        assert_eq!(root.flow.len(), 1);
        assert_eq!(root.sources[0].name, "pg_src");
        assert_eq!(root.sinks[0].name, "pg_sink");
        assert_eq!(root.storages[0].name, "pg_state");
    }

    /// Duplicate component name across `---` documents in one file fires
    /// the same `DuplicateName` error a cross-file collision would —
    /// keeps the "one definition per name, anywhere" rule single-shape.
    #[test]
    fn yaml_multi_document_rejects_duplicate_source_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - { name: pg, type: postgres, config: {} }
---
sources:
  - { name: pg, type: postgres, config: {} }
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(
            matches!(&err, ConfigError::DuplicateName { kind: "source", name } if name == "pg"),
            "got {err:?}"
        );
    }

    /// Same rule applies to flow names: declaring `flow.users` in two
    /// documents of the same file is a `DuplicateFlow`.
    #[test]
    fn yaml_multi_document_rejects_duplicate_flow() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - { name: pg, type: postgres, config: {} }
sinks:
  - { name: pg, type: postgres, config: {} }
storages:
  - { name: pg, type: postgres, config: {} }
flow:
  users:
    source: pg
    sink: pg
    storage: pg
    from: t
    to: t
    mapping: { id: id }
    cursor: { fields: [id] }
---
flow:
  users:
    source: pg
    sink: pg
    storage: pg
    from: t
    to: t
    mapping: { id: id }
    cursor: { fields: [id] }
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(matches!(&err, ConfigError::DuplicateFlow { name } if name == "users"));
    }

    /// Secrets declared across `---` documents in the same file merge
    /// and resolve `${VAR}` expansion across the whole file.
    #[test]
    fn yaml_multi_document_secrets_merge_and_expand() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
secrets:
  PG_HOST: "db.local"
---
secrets:
  PG_PORT: "5432"
---
sources:
  - name: pg
    type: postgres
    config:
      url: "postgres://${PG_HOST}:${PG_PORT}/x"
sinks:
  - { name: pg, type: postgres, config: {} }
storages:
  - { name: pg, type: postgres, config: {} }
"#,
        );
        let root = load(&path).unwrap();
        assert_eq!(
            root.sources[0].config.get("url").and_then(|v| v.as_str()),
            Some("postgres://db.local:5432/x")
        );
    }

    /// Same secret key in two documents of one file is a duplicate, just
    /// like across includes.
    #[test]
    fn yaml_multi_document_rejects_duplicate_secret() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
secrets:
  PG_HOST: "a"
---
secrets:
  PG_HOST: "b"
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(
            matches!(&err, ConfigError::DuplicateSecret { key, .. } if key == "PG_HOST"),
            "got {err:?}"
        );
    }

    /// Wildcard mapping defers the
    /// `cursor.fields ⊆ mapping.from` check to validation. The
    /// loader must accept `cursor=["id"]` + `mapping=["*"]` even
    /// though no concrete `from` is known yet.
    #[test]
    fn wildcard_defers_cursor_subset_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
cursor = { fields = ["id"] }
[flow.f.mapping]
"*" = "*"
"#,
        );
        let root = load(&path).unwrap();
        // Wildcard rule survives to validation as a Short("*") token
        // keyed by the wildcard sink "*".
        let mapping = &root.flow["f"].mapping;
        assert_eq!(mapping.len(), 1);
        let (to, rhs) = mapping.iter().next().expect("one entry");
        assert_eq!(to, "*");
        assert!(matches!(
            rhs,
            crate::config::model::MappingRhs::Short(s) if s == "*"
        ));
    }

    /// Wildcard mapping also defers the
    /// `conflict.key ⊆ mapping.to` check.
    #[test]
    fn wildcard_defers_conflict_key_subset_check() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
cursor = { fields = ["id"] }
conflict = { key = ["id"], strategy = "overwrite" }
[flow.f.mapping]
"*" = "*"
"#,
        );
        let root = load(&path).unwrap();
        assert!(root.flow["f"].conflict.is_some());
    }

    /// `["id", "*"]` together: the explicit
    /// `id` would satisfy the cursor subset check on its own, but
    /// the wildcard's presence still defers the check. We verify
    /// the load succeeds and both rules survive.
    #[test]
    fn wildcard_with_explicit_cursor_field_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
cursor = { fields = ["id"] }
[flow.f.mapping]
id = "id"
"*" = "*"
"#,
        );
        let root = load(&path).unwrap();
        assert_eq!(root.flow["f"].mapping.len(), 2);
    }

    /// YAML/TOML parity for `mapping` containing every shorthand
    /// variant. Both formats must
    /// produce a deeply-equal `flow.mapping` shape.
    #[test]
    fn yaml_toml_mapping_shorthand_parity() {
        use crate::config::model::MappingRhs;

        let dir = tempfile::tempdir().unwrap();
        let toml_path = write(
            dir.path(),
            "config.toml",
            r#"
[[sources]]
name = "pg"
type = "postgres"
config = {}
[[sinks]]
name = "pg"
type = "postgres"
config = {}
[[storages]]
name = "pg"
type = "postgres"
config = {}
[flow.f]
source = "pg"
sink = "pg"
storage = "pg"
from = "t"
to = "t"
cursor = { fields = ["id"] }
[flow.f.mapping]
id = "id"
"*" = "*"
body = "*"
"#,
        );
        let yaml_path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - { name: pg, type: postgres, config: {} }
sinks:
  - { name: pg, type: postgres, config: {} }
storages:
  - { name: pg, type: postgres, config: {} }
flow:
  f:
    source: pg
    sink: pg
    storage: pg
    from: t
    to: t
    cursor: { fields: ["id"] }
    mapping:
      id: "id"
      "*": "*"
      body: "*"
"#,
        );
        let toml_root = load(&toml_path).unwrap();
        let yaml_root = load(&yaml_path).unwrap();

        let toml_mapping = &toml_root.flow["f"].mapping;
        let yaml_mapping = &yaml_root.flow["f"].mapping;
        assert_eq!(toml_mapping.len(), 3);
        assert_eq!(yaml_mapping.len(), 3);
        // Why: assert each rule shape rather than relying on a
        // derived `PartialEq` (the enum wraps `MappingEntry` whose
        // `default: Option<toml::Value>` field doesn't implement
        // `PartialEq` on every dependency version).
        let expected: &[(&str, &str)] = &[("id", "id"), ("*", "*"), ("body", "*")];
        for mapping in [toml_mapping, yaml_mapping] {
            // Map insertion order is parser-dependent (`toml` 1.x does
            // not guarantee preservation), so assert as a set.
            for (to, from) in expected {
                let found = mapping.iter().find(|(k, _)| k == *to).unwrap_or_else(|| {
                    panic!("missing entry for sink {to:?}");
                });
                assert!(
                    matches!(&found.1, MappingRhs::Short(s) if s == from),
                    "entry {to:?} expected Short({from:?}), got {:?}",
                    found.1
                );
            }
        }
    }

    /// Non-string scalar (integer, bool) under a mapping entry must
    /// surface a friendly error from the `MappingRule` visitor naming
    /// both shapes.
    #[test]
    fn mapping_non_string_non_table_rejected_with_friendly_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(
            dir.path(),
            "config.yml",
            r#"
sources:
  - { name: pg, type: postgres, config: {} }
sinks:
  - { name: pg, type: postgres, config: {} }
storages:
  - { name: pg, type: postgres, config: {} }
flow:
  f:
    source: pg
    sink: pg
    storage: pg
    from: t
    to: t
    mapping:
      id: 42
    cursor: { fields: [id] }
"#,
        );
        let err = load(&path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("string") && msg.contains("table"),
            "expected error to mention both shapes; got {msg:?}",
        );
    }
}
