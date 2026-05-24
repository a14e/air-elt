use std::path::Path;
use std::process::Command;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct VersionCheck;

/// Ensures the workspace version has been bumped compared to origin/main.
/// Shells out to `git show origin/main:Cargo.toml` to read the base version,
/// then compares via semver. Automatically skipped on the main/master branch
/// (detected via CI env vars or git).
impl Check for VersionCheck {
    fn name(&self) -> &'static str {
        "version"
    }

    fn run(&self, root: &Path, workspace: &Workspace) -> Vec<Finding> {
        if is_main_branch(root) {
            return Vec::new();
        }

        let mut findings = Vec::new();

        let current = match semver::Version::parse(&workspace.version) {
            Ok(version) => version,
            Err(error) => {
                findings.push(Finding::error(
                    "Cargo.toml",
                    None,
                    format!(
                        "workspace version '{}' is not valid semver: {error}",
                        workspace.version
                    ),
                ));
                return findings;
            }
        };

        let output = Command::new("git")
            .args(["show", "origin/main:Cargo.toml"])
            .current_dir(root)
            .output();

        let output = match output {
            Ok(output) if output.status.success() => output,
            _ => {
                findings.push(Finding::warning(
                    "Cargo.toml",
                    None,
                    "cannot read origin/main:Cargo.toml — skipping version bump check",
                ));
                return findings;
            }
        };

        let base_content = String::from_utf8_lossy(&output.stdout);
        let base_doc: toml::Value = match toml::from_str(&base_content) {
            Ok(doc) => doc,
            Err(error) => {
                findings.push(Finding::warning(
                    "Cargo.toml",
                    None,
                    format!("cannot parse origin/main:Cargo.toml: {error}"),
                ));
                return findings;
            }
        };

        let base_version_string = base_doc
            .get("workspace")
            .and_then(|workspace| workspace.get("package"))
            .and_then(|package| package.get("version"))
            .and_then(|version| version.as_str())
            .unwrap_or("0.0.0");

        let base = match semver::Version::parse(base_version_string) {
            Ok(version) => version,
            Err(error) => {
                findings.push(Finding::warning(
                    "Cargo.toml",
                    None,
                    format!(
                        "origin/main version '{base_version_string}' is not valid semver: {error}"
                    ),
                ));
                return findings;
            }
        };

        if current <= base {
            findings.push(Finding::error(
                "Cargo.toml",
                None,
                format!("workspace version {current} must be higher than origin/main ({base})"),
            ));
        }

        findings
    }
}

fn is_main_branch(root: &Path) -> bool {
    // GitHub Actions
    if let Ok(ref_name) = std::env::var("GITHUB_REF_NAME") {
        return ref_name == "main" || ref_name == "master";
    }
    // GitLab CI
    if let Ok(branch) = std::env::var("CI_COMMIT_BRANCH") {
        return branch == "main" || branch == "master";
    }
    // Bitbucket Pipelines
    if let Ok(branch) = std::env::var("BITBUCKET_BRANCH") {
        return branch == "main" || branch == "master";
    }
    // Generic CI — try git
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(root)
        .output();
    if let Ok(output) = output {
        if output.status.success() {
            let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return branch == "main" || branch == "master";
        }
    }
    false
}
