use std::path::Path;

use walkdir::WalkDir;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct ContainersCheck;

/// Ensures every testcontainer .start() call has a preceding .with_container_name().
/// Named containers are required for ryuk to clean them up after test runs.
/// Scans commons/testing/src/*.rs (excluding ryuk.rs itself).
impl Check for ContainersCheck {
    fn name(&self) -> &'static str {
        "containers"
    }

    fn run(&self, root: &Path, _workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        let testing_dir = root.join("crates/commons/testing/src");
        if !testing_dir.is_dir() {
            return findings;
        }

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

            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            // ryuk manages other containers' lifecycle — it doesn't need with_container_name itself
            if file_name == "ryuk.rs" {
                continue;
            }

            check_container_naming(&content, &relative.display().to_string(), &mut findings);
        }

        findings
    }
}

fn check_container_naming(content: &str, file_path: &str, findings: &mut Vec<Finding>) {
    let lines: Vec<&str> = content.lines().collect();

    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if !trimmed.contains(".start(") {
            continue;
        }
        // Skip lines in comments
        if trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*') {
            continue;
        }

        let line_number = index + 1;
        let has_container_name = scan_backward_for_container_name(&lines, index);

        if !has_container_name {
            findings.push(Finding::error(
                file_path,
                Some(line_number),
                "container .start() without .with_container_name() — ryuk needs named containers",
            ));
        }
    }
}

fn scan_backward_for_container_name(lines: &[&str], start_line: usize) -> bool {
    let search_start = start_line.saturating_sub(30);
    for index in (search_start..=start_line).rev() {
        if lines[index].contains("with_container_name") {
            return true;
        }
        // Stop scanning if we hit a function boundary or empty line after code
        let trimmed = lines[index].trim();
        if trimmed.starts_with("pub fn ")
            || trimmed.starts_with("fn ")
            || trimmed.starts_with("pub async fn ")
            || trimmed.starts_with("async fn ")
        {
            break;
        }
    }
    false
}
