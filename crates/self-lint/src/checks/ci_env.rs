use std::path::Path;

use regex::Regex;
use walkdir::WalkDir;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct CiEnvCheck;

/// Ensures every AIR_ELT_TEST_* env var used in commons/testing is declared in CI.
/// Extracts var names via regex from test code, then checks they appear in ci.yml.
/// Optional vars (AIR_ELT_TEST_SESSION_ID) are exempt.
impl Check for CiEnvCheck {
    fn name(&self) -> &'static str {
        "ci-env"
    }

    fn run(&self, root: &Path, _workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        let testing_dir = root.join("crates/commons/testing/src");
        let ci_path = root.join(".github/workflows/ci.yml");

        let ci_content = match std::fs::read_to_string(&ci_path) {
            Ok(content) => content,
            Err(error) => {
                findings.push(Finding::warning(
                    ".github/workflows/ci.yml",
                    None,
                    format!("cannot read CI workflow: {error}"),
                ));
                return findings;
            }
        };

        let env_pattern = Regex::new(r#""(AIR_ELT_TEST_[A-Z_]+)""#).expect("static regex");

        let mut code_env_vars: Vec<(String, String)> = Vec::new();

        for entry in WalkDir::new(&testing_dir).into_iter().flatten() {
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "rs") {
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(_) => continue,
            };

            let relative = path.strip_prefix(root).unwrap_or(path);

            for captures in env_pattern.captures_iter(&content) {
                let var_name = captures.get(1).expect("capture group").as_str().to_string();
                code_env_vars.push((var_name, relative.display().to_string()));
            }
        }

        code_env_vars.sort_by(|a, b| a.0.cmp(&b.0));
        code_env_vars.dedup_by(|a, b| a.0 == b.0);

        let optional_vars = ["AIR_ELT_TEST_SESSION_ID"];

        for (var_name, source_file) in &code_env_vars {
            if optional_vars.contains(&var_name.as_str()) {
                continue;
            }
            if !ci_content.contains(var_name.as_str()) {
                findings.push(Finding::error(
                    source_file,
                    None,
                    format!("env var {var_name} used in test code but not set in CI workflow"),
                ));
            }
        }

        findings
    }
}
