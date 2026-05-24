use std::path::Path;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct DoctestCheck;

/// Ensures every lib crate disables doctests via [lib] doctest = false in Cargo.toml.
/// Doctests are disabled workspace-wide to avoid accidental doc-test failures.
impl Check for DoctestCheck {
    fn name(&self) -> &'static str {
        "doctest"
    }

    fn run(&self, _root: &Path, workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        for crate_info in workspace.crates.values() {
            if !crate_info.has_lib {
                continue;
            }

            if !crate_info.doctest_disabled {
                findings.push(Finding::error(
                    format!("{}/Cargo.toml", crate_info.relative_path),
                    None,
                    format!(
                        "crate '{}' is missing [lib] doctest = false",
                        crate_info.name
                    ),
                ));
            }
        }

        findings
    }
}
