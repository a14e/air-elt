use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::debug;

use crate::config::env_expand;
use crate::config::model::{MappingEntry, RootConfig};
use crate::error::ConfigError;

/// Hard cap on a single config file. Operator-controlled files in MVP are
/// typically a few KB; 16 MiB is comfortably above anything real and low
/// enough that a pathological `include = ["/"]` shouldn't OOM the process.
const MAX_CONFIG_BYTES: u64 = 16 * 1024 * 1024;

/// Load the root config from `path`, merge any files listed in
/// `[config].include`, and expand `${VAR}` / `${VAR:default}` references.
///
/// Include entries are **relative to the including file's directory**;
/// absolute paths are rejected so operators can't accidentally walk the whole
/// filesystem. Directories are read non-recursively for `*.toml` files in
/// sorted order. Canonical paths are tracked in a HashSet so symlink loops
/// visit each real file at most once.
pub fn load<P: AsRef<Path>>(path: P) -> Result<RootConfig, ConfigError> {
    let path = path.as_ref().to_path_buf();
    let mut cycle = PathCycleDetector::new();

    let raw_root = read_single(&path)?;
    cycle.mark(&path);
    let secrets = extract_secrets(&raw_root, &path)?;
    let expanded_root = env_expand::expand(&raw_root, &secrets)?;
    let mut root = parse_root(&expanded_root, &path)?;

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
        merge_include(&mut root, &resolved, &secrets, &mut cycle)?;
    }

    validate_post_merge(&root)?;
    Ok(root)
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

fn parse_root(expanded: &str, path: &Path) -> Result<RootConfig, ConfigError> {
    toml::from_str::<RootConfig>(expanded).map_err(|source| ConfigError::TomlParse {
        path: path.to_path_buf(),
        source,
    })
}

/// A minimal first-pass parse that only extracts `[secrets]`. We need secrets
/// before we expand the rest of the file, but the rest of the file can contain
/// `${VAR}` references in otherwise-typed positions (numbers, enums) which
/// would fail the stricter final parse — so we explicitly opt into a
/// permissive shape here.
fn extract_secrets(raw: &str, path: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
    #[derive(Deserialize)]
    struct SecretsOnly {
        #[serde(default)]
        secrets: BTreeMap<String, String>,
    }
    let parsed: SecretsOnly = toml::from_str(raw).map_err(|source| ConfigError::TomlParse {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(parsed.secrets)
}

struct PathCycleDetector {
    visited: HashSet<PathBuf>,
}

impl PathCycleDetector {
    fn new() -> Self {
        Self {
            visited: HashSet::new(),
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
) -> Result<(), ConfigError> {
    if include.is_dir() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(include)
            .map_err(|source| ConfigError::Io {
                path: include.to_path_buf(),
                source,
            })?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        entries.sort();
        for entry in entries {
            if cycle.seen(&entry) {
                debug!(path = ?entry, "skipping already-visited include file");
                continue;
            }
            merge_file(root, &entry, secrets, cycle)?;
        }
        Ok(())
    } else {
        if cycle.seen(include) {
            debug!(path = ?include, "skipping already-visited include file");
            return Ok(());
        }
        merge_file(root, include, secrets, cycle)
    }
}

fn merge_file(
    root: &mut RootConfig,
    path: &Path,
    secrets: &BTreeMap<String, String>,
    cycle: &mut PathCycleDetector,
) -> Result<(), ConfigError> {
    cycle.mark(path);
    let raw = read_single(path)?;
    let expanded = env_expand::expand(&raw, secrets)?;
    let extra = parse_root(&expanded, path)?;

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
    // Secrets declared in an included file are accepted — first writer wins,
    // which matches intuition about "include is additive, not overriding".
    for (k, v) in extra.secrets {
        root.secrets.entry(k).or_insert(v);
    }
    Ok(())
}

/// Structural checks after all files are merged.
fn validate_post_merge(root: &RootConfig) -> Result<(), ConfigError> {
    for (flow_name, flow) in &root.flow {
        for entry in &flow.mapping {
            if let MappingEntry::Object(obj) = entry
                && (obj.from.transform.is_some()
                    || obj.from.timezone.is_some()
                    || obj.from.data_type.is_some())
            {
                return Err(ConfigError::UnsupportedInMvp {
                    what: format!(
                        "mapping transform/timezone/data_type on field {:?} of flow {flow_name:?}",
                        obj.from.name
                    ),
                });
            }
        }
        if flow.cursor.fields.is_empty() {
            return Err(ConfigError::Invalid {
                reason: format!("flow {flow_name:?} has empty cursor.fields"),
            });
        }
        if flow.batch_limit == 0 {
            return Err(ConfigError::Invalid {
                reason: format!("flow {flow_name:?} has batch-limit = 0"),
            });
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
        let mapped_froms: HashSet<&str> = flow
            .mapping
            .iter()
            .map(|m| match m {
                MappingEntry::Simple(s) => s.from.as_str(),
                MappingEntry::Object(o) => o.from.name.as_str(),
            })
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

    #[test]
    fn transform_in_mapping_rejected_in_mvp() {
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
mapping = [{ from = { name = "created_at", transform = "seconds" }, to = "created_at" }]
cursor = { fields = ["created_at"] }
"#,
        );
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::UnsupportedInMvp { .. }));
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
