use std::path::Path;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct AppTestsCheck;

/// Ensures every registered connector type has end-to-end test coverage in app/tests/.
/// Extracts connector keys from the registry and checks that each appears in at least
/// one test filename (with hyphen-to-underscore normalization).
impl Check for AppTestsCheck {
    fn name(&self) -> &'static str {
        "app-tests"
    }

    fn run(&self, root: &Path, workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        let registry_path = root.join("crates/app/src/registry.rs");
        let registry_content = match std::fs::read_to_string(&registry_path) {
            Ok(content) => content,
            Err(_) => return findings,
        };

        let pattern = regex::Regex::new(r#"register_(?:source|sink|storage)\("([^"]+)""#)
            .expect("static regex");

        let mut connector_keys: Vec<String> = pattern
            .captures_iter(&registry_content)
            .map(|captures| captures.get(1).expect("capture group").as_str().to_string())
            .collect();
        connector_keys.sort();
        connector_keys.dedup();

        let tests_dir = root.join("crates/app/tests");
        let test_filenames = collect_test_filenames(&tests_dir);

        let _ = workspace;

        for key in &connector_keys {
            let normalized_key = key.replace('-', "_");
            let found = test_filenames.iter().any(|filename| {
                filename.contains(&normalized_key) || matches_short_form(filename, key)
            });
            if !found {
                findings.push(Finding::error(
                    "crates/app/tests",
                    None,
                    format!("connector type '{key}' has no test coverage in app/tests/"),
                ));
            }
        }

        findings
    }
}

fn matches_short_form(filename: &str, key: &str) -> bool {
    match key {
        "postgres" => filename.contains("pg_") || filename.contains("_pg"),
        "mongodb" => filename.contains("mongo_") || filename.contains("_mongo"),
        _ => false,
    }
}

fn collect_test_filenames(tests_dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(tests_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.ends_with(".rs") {
                Some(filename)
            } else {
                None
            }
        })
        .collect()
}
