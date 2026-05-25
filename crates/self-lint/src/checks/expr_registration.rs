use std::path::Path;

use regex::Regex;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct ExprRegistrationCheck;

/// Verifies that every static function declaration in `crates/expr/funcs/src/builtins/*.rs`
/// has a matching `registry.register(&NAME)` call, and that no `Arc::new(` patterns exist
/// in register functions (all should use static refs).
impl Check for ExprRegistrationCheck {
    fn name(&self) -> &'static str {
        "expr-registration"
    }

    fn run(&self, root: &Path, _workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();
        let builtins_dir = root.join("crates/expr/funcs/src/builtins");

        if !builtins_dir.is_dir() {
            return findings;
        }

        let static_re = Regex::new(r"(?m)^static\s+(\w+)\s*:").expect("static regex");
        let register_re = Regex::new(r"registry\.register\(&(\w+)\)").expect("register regex");

        for entry in std::fs::read_dir(&builtins_dir)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            if path.file_name().is_none_or(|n| n == "mod.rs") {
                continue;
            }

            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .display()
                .to_string();

            // Find all static declarations
            let statics: Vec<&str> = static_re
                .captures_iter(&content)
                .map(|c| c.get(1).expect("group").as_str())
                .collect();
            let registered: Vec<&str> = register_re
                .captures_iter(&content)
                .map(|c| c.get(1).expect("group").as_str())
                .collect();

            for name in &statics {
                if !registered.contains(name) {
                    findings.push(Finding::error(
                        &relative,
                        None,
                        format!("static function '{name}' is defined but not registered"),
                    ));
                }
            }

            // Check for Arc::new usage in production code (outside test modules)
            if content.contains("Arc::new(") {
                let test_start = content.find("#[cfg(test)]").unwrap_or(content.len());
                let before_tests = &content[..test_start];
                if before_tests.contains("Arc::new(") {
                    findings.push(Finding::error(
                        &relative,
                        None,
                        "production code uses Arc::new — use static refs instead".to_string(),
                    ));
                }
            }
        }

        findings
    }
}
