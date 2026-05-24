use std::path::Path;

use walkdir::WalkDir;

use crate::diagnostic::{Check, Finding};
use crate::workspace::{CrateCategory, Workspace};

struct TraitRequirement {
    pattern: &'static str,
    label: &'static str,
}

const SOURCE_REQUIREMENTS: &[TraitRequirement] = &[
    TraitRequirement {
        pattern: "impl Source for",
        label: "Source",
    },
    TraitRequirement {
        pattern: "impl SourceFactory for",
        label: "SourceFactory",
    },
    TraitRequirement {
        pattern: "impl SourceCtx for",
        label: "SourceCtx",
    },
];

const SINK_REQUIREMENTS: &[TraitRequirement] = &[
    TraitRequirement {
        pattern: "impl Sink for",
        label: "Sink",
    },
    TraitRequirement {
        pattern: "impl SinkFactory for",
        label: "SinkFactory",
    },
    TraitRequirement {
        pattern: "impl SinkCtx for",
        label: "SinkCtx",
    },
];

const STORAGE_REQUIREMENTS: &[TraitRequirement] = &[
    TraitRequirement {
        pattern: "impl Storage for",
        label: "Storage",
    },
    TraitRequirement {
        pattern: "impl StorageFactory for",
        label: "StorageFactory",
    },
];

pub struct TraitsCheck;

/// Ensures each connector crate implements the required traits.
/// Sources must have Source + SourceFactory + SourceCtx; sinks must have Sink + SinkFactory + SinkCtx;
/// storages must have Storage + StorageFactory. Greps source files for `impl Trait for` patterns.
impl Check for TraitsCheck {
    fn name(&self) -> &'static str {
        "traits"
    }

    fn run(&self, root: &Path, workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        for crate_info in workspace.crates_by_category(CrateCategory::Source) {
            check_crate_traits(
                root,
                &crate_info.relative_path,
                &crate_info.name,
                SOURCE_REQUIREMENTS,
                &mut findings,
            );
        }
        for crate_info in workspace.crates_by_category(CrateCategory::Sink) {
            check_crate_traits(
                root,
                &crate_info.relative_path,
                &crate_info.name,
                SINK_REQUIREMENTS,
                &mut findings,
            );
        }
        for crate_info in workspace.crates_by_category(CrateCategory::Storage) {
            check_crate_traits(
                root,
                &crate_info.relative_path,
                &crate_info.name,
                STORAGE_REQUIREMENTS,
                &mut findings,
            );
        }

        findings
    }
}

fn check_crate_traits(
    root: &Path,
    relative_path: &str,
    crate_name: &str,
    requirements: &[TraitRequirement],
    findings: &mut Vec<Finding>,
) {
    let src_dir = root.join(relative_path).join("src");
    let all_source = collect_source_content(&src_dir);

    for requirement in requirements {
        if !all_source.contains(requirement.pattern) {
            findings.push(Finding::error(
                format!("{relative_path}/src"),
                None,
                format!("crate '{crate_name}' is missing `{}`", requirement.label),
            ));
        }
    }
}

fn collect_source_content(src_dir: &Path) -> String {
    let mut content = String::new();
    for entry in WalkDir::new(src_dir).into_iter().flatten() {
        if entry.file_type().is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "rs")
        {
            if let Ok(file_content) = std::fs::read_to_string(entry.path()) {
                content.push_str(&file_content);
                content.push('\n');
            }
        }
    }
    content
}
