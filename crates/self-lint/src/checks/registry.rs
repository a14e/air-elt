use std::path::Path;

use regex::Regex;

use crate::diagnostic::{Check, Finding};
use crate::workspace::{CrateCategory, Workspace};

pub struct RegistryCheck;

/// Ensures every source/sink/storage crate has at least one factory registered in app.
/// Parses app/src/registry.rs for register_source/sink/storage calls and cross-references
/// with the workspace crate list.
impl Check for RegistryCheck {
    fn name(&self) -> &'static str {
        "registry"
    }

    fn run(&self, root: &Path, workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        let registry_path = root.join("crates/app/src/registry.rs");
        let content = match std::fs::read_to_string(&registry_path) {
            Ok(content) => content,
            Err(error) => {
                findings.push(Finding::error(
                    "crates/app/src/registry.rs",
                    None,
                    format!("cannot read registry file: {error}"),
                ));
                return findings;
            }
        };

        let pattern =
            Regex::new(r#"register_(source|sink|storage)\("([^"]+)""#).expect("static regex");

        let mut registered_sources = Vec::new();
        let mut registered_sinks = Vec::new();
        let mut registered_storages = Vec::new();

        for captures in pattern.captures_iter(&content) {
            let kind = captures.get(1).expect("capture group").as_str();
            let key = captures.get(2).expect("capture group").as_str().to_string();
            match kind {
                "source" => registered_sources.push(key),
                "sink" => registered_sinks.push(key),
                "storage" => registered_storages.push(key),
                _ => {}
            }
        }

        check_category_registered(
            workspace,
            CrateCategory::Source,
            &registered_sources,
            "source",
            &mut findings,
        );
        check_category_registered(
            workspace,
            CrateCategory::Sink,
            &registered_sinks,
            "sink",
            &mut findings,
        );
        check_category_registered(
            workspace,
            CrateCategory::Storage,
            &registered_storages,
            "storage",
            &mut findings,
        );

        findings
    }
}

fn check_category_registered(
    workspace: &Workspace,
    category: CrateCategory,
    registered_keys: &[String],
    kind_label: &str,
    findings: &mut Vec<Finding>,
) {
    for crate_info in workspace.crates_by_category(category) {
        let crate_dir_name = crate_info
            .relative_path
            .rsplit('/')
            .next()
            .unwrap_or(&crate_info.relative_path);

        let has_registration = registered_keys
            .iter()
            .any(|key| key == crate_dir_name || content_matches_crate(key, crate_dir_name));

        if !has_registration {
            findings.push(Finding::error(
                "crates/app/src/registry.rs",
                None,
                format!(
                    "{kind_label} crate '{}' ({crate_dir_name}) has no factory registered in build_registry()",
                    crate_info.name
                ),
            ));
        }
    }
}

fn content_matches_crate(registry_key: &str, crate_dir_name: &str) -> bool {
    // cockroachdb uses the postgres crate
    if registry_key == "cockroachdb" && crate_dir_name == "postgres" {
        return true;
    }
    // mongo-cdc directory name matches registry key
    if registry_key.replace('-', "_") == crate_dir_name.replace('-', "_") {
        return true;
    }
    false
}
