use std::path::Path;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct TestAggregatorCheck;

/// Ensures every crate with a tests/ directory consolidates tests into a single binary.
/// Checks for tests/all.rs, autotests = false, and [[test]] name = "all" in Cargo.toml.
/// This convention cuts cargo's per-binary serialization overhead.
impl Check for TestAggregatorCheck {
    fn name(&self) -> &'static str {
        "test-aggregator"
    }

    fn run(&self, _root: &Path, workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        for crate_info in workspace.crates.values() {
            if !crate_info.has_tests_directory {
                continue;
            }

            let cargo_path = format!("{}/Cargo.toml", crate_info.relative_path);
            let all_rs_path = crate_info.path.join("tests/all.rs");

            if !all_rs_path.exists() {
                findings.push(Finding::error(
                    format!("{}/tests", crate_info.relative_path),
                    None,
                    format!(
                        "crate '{}' has tests/ directory but no tests/all.rs",
                        crate_info.name
                    ),
                ));
            }

            if !crate_info.autotests_disabled {
                findings.push(Finding::error(
                    &cargo_path,
                    None,
                    format!(
                        "crate '{}' has tests/ but autotests is not disabled",
                        crate_info.name
                    ),
                ));
            }

            if !crate_info.has_test_all {
                findings.push(Finding::error(
                    &cargo_path,
                    None,
                    format!(
                        "crate '{}' is missing [[test]] with name = \"all\" and path = \"tests/all.rs\"",
                        crate_info.name
                    ),
                ));
            }
        }

        findings
    }
}
