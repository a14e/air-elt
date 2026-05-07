use std::collections::BTreeMap;

use ahash::AHashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::debug;

use crate::config::env_expand;
use crate::config::model::RootConfig;
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
        ConfigFormat::Yaml => {
            serde_yaml::from_str::<RootConfig>(expanded).map_err(|source| ConfigError::YamlParse {
                path: path.to_path_buf(),
                source,
            })
        }
    }
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
    #[derive(Deserialize)]
    struct SecretsOnly {
        #[serde(default)]
        secrets: BTreeMap<String, String>,
    }
    let parsed: SecretsOnly = match format {
        ConfigFormat::Toml => toml::from_str(raw).map_err(|source| ConfigError::TomlParse {
            path: path.to_path_buf(),
            source,
        })?,
        ConfigFormat::Yaml => {
            serde_yaml::from_str(raw).map_err(|source| ConfigError::YamlParse {
                path: path.to_path_buf(),
                source,
            })?
        }
    };
    Ok(parsed.secrets)
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
        // Why: Postgres rejects statements with more than 65 535 bind parameters
        // (wire protocol uses u16 for bind-count). A sink batch of N rows over
        // C mapped columns emits N*C binds; we cap below the hard limit so
        // operators see a clear error at validate rather than sqlx complaining
        // mid-drain. Source SELECTs only bind cursor fields per batch, so the
        // check is guided by sink shape.
        let cols = flow.mapping.len();
        if flow.batch_limit.saturating_mul(cols) > 60_000 {
            return Err(ConfigError::Invalid {
                reason: format!(
                    "flow {flow_name:?}: batch_limit={} × mapping_cols={} exceeds 60_000 bind parameters",
                    flow.batch_limit, cols
                ),
            });
        }
        // Cursor fields must appear in mapping.from — otherwise the source
        // SELECT will not project them and runtime will emit a misleading
        // error. This used to be a silent dead-fallback in pg_source.
        let mapped_froms: AHashSet<&str> = flow.mapping.iter().map(|m| m.from.as_str()).collect();
        for cf in &flow.cursor.fields {
            if !mapped_froms.contains(cf.as_str()) {
                return Err(ConfigError::Invalid {
                    reason: format!(
                        "flow {flow_name:?}: cursor field {cf:?} must be listed in mapping.from"
                    ),
                });
            }
        }
        if let Some(conflict) = &flow.conflict {
            conflict.validate().map_err(|reason| ConfigError::Invalid {
                reason: format!("flow {flow_name:?}: {reason}"),
            })?;
            // Every conflict.key entry must appear in mapping.to —
            // otherwise the upsert filter would reference a sink column
            // we never write.
            let mapped_tos: AHashSet<&str> = flow.mapping.iter().map(|m| m.to.as_str()).collect();
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
mapping = [
    { from = "id", to = "id" },
    { from = "name", to = "name" },
]
cursor = { fields = ["id"], order = "asc", interval = "1s" }
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["created_at"] }
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
            mapping_lines.push_str(&format!("    {{ from = \"c{i}\", to = \"c{i}\" }},\n"));
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
mapping = [
{mapping_lines}]
cursor = {{ fields = ["c0"] }}
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"], interval = "0s" }
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
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
      - { from: id, to: id }
      - { from: name, to: name }
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
    mapping: [{ from: id, to: id }]
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
      - { from: id, to: id }
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
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
    mapping: [{ from: id, to: id }]
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
    mapping: [{ from: id, to: id }]
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
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
    mapping: [{ from: id, to: id }]
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
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
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
"#,
        );
        let err = load(dir.path().join("config.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::DuplicateFlow { .. }));
    }
}
