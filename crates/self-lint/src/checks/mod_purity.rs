use std::path::Path;

use walkdir::WalkDir;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

const SKIPPED_DIRECTORIES: &[&str] = &["target", ".git"];

pub struct ModPurityCheck;

/// Enforces the pure-package convention: mod.rs and lib.rs must contain only
/// module declarations, use/pub-use statements, attributes, and comments.
/// No functions, structs, impls, or other logic. Uses a brace-depth state machine
/// to handle multi-line use blocks.
impl Check for ModPurityCheck {
    fn name(&self) -> &'static str {
        "mod-purity"
    }

    fn run(&self, root: &Path, _workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        let crates_dir = root.join("crates");

        for entry in WalkDir::new(&crates_dir).into_iter().filter_entry(|entry| {
            let file_name = entry.file_name().to_string_lossy();
            !SKIPPED_DIRECTORIES.contains(&file_name.as_ref())
        }) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            let file_name = path
                .file_name()
                .and_then(|file_name| file_name.to_str())
                .unwrap_or("");

            if file_name != "mod.rs" && file_name != "lib.rs" {
                continue;
            }

            // Skip test directories
            if path
                .components()
                .any(|component| component.as_os_str() == "tests")
            {
                continue;
            }

            let content = match std::fs::read_to_string(path) {
                Ok(content) => content,
                Err(_) => continue,
            };

            let relative = path.strip_prefix(root).unwrap_or(path);
            check_file_purity(&content, &relative.display().to_string(), &mut findings);
        }

        findings
    }
}

fn check_file_purity(content: &str, file_path: &str, findings: &mut Vec<Finding>) {
    let mut brace_depth: i32 = 0;
    let mut in_attribute = false;

    for (index, line) in content.lines().enumerate() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        // Track multi-line attributes
        if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
            in_attribute = true;
        }
        if in_attribute {
            brace_depth += count_char(trimmed, '(') - count_char(trimmed, ')');
            brace_depth += count_char(trimmed, '[') - count_char(trimmed, ']');
            if brace_depth <= 0 {
                in_attribute = false;
                brace_depth = 0;
            }
            continue;
        }

        // Track multi-line use statements
        if brace_depth > 0 {
            brace_depth += count_char(trimmed, '{') - count_char(trimmed, '}');
            continue;
        }

        if is_allowed_line(trimmed) {
            brace_depth += count_char(trimmed, '{') - count_char(trimmed, '}');
            continue;
        }

        let line_number = index + 1;
        findings.push(Finding::error(
            file_path,
            Some(line_number),
            format!(
                "mod.rs/lib.rs must contain only imports and module declarations, found: {}",
                truncate(trimmed, 60)
            ),
        ));
        return;
    }
}

fn is_allowed_line(trimmed: &str) -> bool {
    // Comments
    if trimmed.starts_with("//") {
        return true;
    }

    // Module declarations
    if trimmed.starts_with("mod ")
        || trimmed.starts_with("pub mod ")
        || trimmed.starts_with("pub(crate) mod ")
    {
        return true;
    }

    // Use statements
    if trimmed.starts_with("use ")
        || trimmed.starts_with("pub use ")
        || trimmed.starts_with("pub(crate) use ")
    {
        return true;
    }

    // Attributes
    if trimmed.starts_with("#[") || trimmed.starts_with("#![") {
        return true;
    }

    // Closing braces from multi-line use/attribute
    if trimmed == "}" || trimmed == "};" {
        return true;
    }

    false
}

fn count_char(string: &str, character: char) -> i32 {
    string.chars().filter(|&c| c == character).count() as i32
}

fn truncate(string: &str, max_length: usize) -> &str {
    if string.len() <= max_length {
        string
    } else {
        &string[..max_length]
    }
}
