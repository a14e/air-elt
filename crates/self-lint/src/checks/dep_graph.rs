use std::path::Path;

use crate::diagnostic::{Check, Finding};
use crate::workspace::{CrateCategory, Workspace};

pub struct DepGraphCheck;

/// Enforces the layered dependency architecture: each crate category has strict rules
/// about which other categories it may depend on (e.g., commons cannot depend on sinks).
/// Parses Cargo.toml [dependencies] for air-elt-* entries and validates against the category matrix.
impl Check for DepGraphCheck {
    fn name(&self) -> &'static str {
        "dep-graph"
    }

    fn run(&self, _root: &Path, workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        for crate_info in workspace.crates.values() {
            let Some(source_category) = crate_info.category else {
                continue;
            };
            let cargo_path = format!("{}/Cargo.toml", crate_info.relative_path);

            for dep_name in &crate_info.dependencies {
                let Some(dep_info) = workspace.crates.get(dep_name.as_str()) else {
                    continue;
                };
                let Some(target_category) = dep_info.category else {
                    continue;
                };

                let violation = match source_category {
                    CrateCategory::Foundation => {
                        matches!(
                            target_category,
                            CrateCategory::Expression
                                | CrateCategory::Core
                                | CrateCategory::Source
                                | CrateCategory::Sink
                                | CrateCategory::Storage
                                | CrateCategory::App
                                | CrateCategory::Monitoring
                                | CrateCategory::CommonsDb
                                | CrateCategory::CommonsTesting
                        )
                    }
                    CrateCategory::Expression => !matches!(
                        target_category,
                        CrateCategory::Foundation | CrateCategory::Expression
                    ),
                    CrateCategory::Monitoring => {
                        !matches!(target_category, CrateCategory::Foundation)
                    }
                    CrateCategory::Core => matches!(
                        target_category,
                        CrateCategory::Source
                            | CrateCategory::Sink
                            | CrateCategory::Storage
                            | CrateCategory::App
                            | CrateCategory::CommonsDb
                    ),
                    CrateCategory::CommonsDb | CrateCategory::CommonsTesting => matches!(
                        target_category,
                        CrateCategory::Source
                            | CrateCategory::Sink
                            | CrateCategory::Storage
                            | CrateCategory::App
                    ),
                    CrateCategory::Source => matches!(
                        target_category,
                        CrateCategory::Source
                            | CrateCategory::Sink
                            | CrateCategory::Storage
                            | CrateCategory::App
                    ),
                    CrateCategory::Sink => matches!(
                        target_category,
                        CrateCategory::Source
                            | CrateCategory::Sink
                            | CrateCategory::Storage
                            | CrateCategory::App
                    ),
                    CrateCategory::Storage => matches!(
                        target_category,
                        CrateCategory::Source
                            | CrateCategory::Sink
                            | CrateCategory::Storage
                            | CrateCategory::App
                    ),
                    CrateCategory::SelfLint => true,
                    CrateCategory::App => false,
                };

                if violation {
                    findings.push(Finding::error(
                        &cargo_path,
                        None,
                        format!(
                            "{} ({source_category:?}) must not depend on {} ({target_category:?})",
                            crate_info.name, dep_name
                        ),
                    ));
                }
            }

            if crate_info
                .dependencies
                .iter()
                .any(|dep| dep == "air-elt-commons-testing")
            {
                findings.push(Finding::error(
                    &cargo_path,
                    None,
                    format!(
                        "{} has air-elt-commons-testing in [dependencies] — it must be dev-only",
                        crate_info.name
                    ),
                ));
            }

            if crate_info
                .dependencies
                .iter()
                .any(|dep| dep == "air-elt-app")
            {
                findings.push(Finding::error(
                    &cargo_path,
                    None,
                    format!(
                        "{} depends on air-elt-app — nothing should depend on app",
                        crate_info.name
                    ),
                ));
            }
        }

        findings
    }
}
