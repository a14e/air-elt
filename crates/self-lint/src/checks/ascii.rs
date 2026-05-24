use std::path::Path;

use walkdir::WalkDir;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

const CHECKED_EXTENSIONS: &[&str] = &["rs", "toml", "yml", "yaml", "md"];

const SKIPPED_DIRECTORIES: &[&str] = &[
    "target",
    ".git",
    "node_modules",
    "for-future-ignore-that",
    "worktrees",
];

pub struct NonEnglishCheck;

/// Enforces the English-only rule: all project files must use Latin-script characters.
/// Scans .rs, .toml, .yml, .yaml, .md files for non-Latin Unicode (Cyrillic, CJK, Arabic, etc.).
/// Typographic symbols, accented Latin letters, and emoji are allowed.
/// Test files and #[cfg(test)] sections are exempt.
impl Check for NonEnglishCheck {
    fn name(&self) -> &'static str {
        "non-english"
    }

    fn run(&self, root: &Path, _workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        for entry in WalkDir::new(root).into_iter().filter_entry(|entry| {
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
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("");
            if !CHECKED_EXTENSIONS.contains(&extension) {
                continue;
            }

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

            let mut in_test_section = false;
            for (index, line) in content.lines().enumerate() {
                if line.contains("#[cfg(test)]") {
                    in_test_section = true;
                }
                if in_test_section {
                    continue;
                }
                if let Some(character) = find_non_latin_character(line) {
                    findings.push(Finding::error(
                        relative.display().to_string(),
                        Some(index + 1),
                        format!(
                            "non-Latin character '\\u{{{:04X}}}' ({character}) found — all files must be in English",
                            character as u32
                        ),
                    ));
                    break;
                }
            }
        }

        findings
    }
}

fn find_non_latin_character(line: &str) -> Option<char> {
    for character in line.chars() {
        if character.is_ascii() {
            continue;
        }
        if is_allowed_unicode(character) {
            continue;
        }
        return Some(character);
    }
    None
}

fn is_allowed_unicode(character: char) -> bool {
    matches!(character,
        '\u{2013}'..='\u{2015}' |
        '\u{2018}'..='\u{201F}' |
        '\u{2026}' |
        '\u{2022}' | '\u{2023}' |
        '\u{00AB}' | '\u{00BB}' |
        '\u{2032}'..='\u{2037}' |
        '\u{00A0}' |
        '\u{00B7}' |
        '\u{2010}'..='\u{2012}' |
        '\u{00C0}'..='\u{00FF}' |
        '\u{0100}'..='\u{024F}' |
        '\u{00A7}' | '\u{00B0}' | '\u{00B5}' |
        '\u{00B1}' |
        '\u{2190}'..='\u{21FF}' |
        '\u{2200}'..='\u{22FF}' |
        '\u{2300}'..='\u{23FF}' |
        '\u{25A0}'..='\u{25FF}' |
        '\u{2600}'..='\u{26FF}' |
        '\u{00B2}' | '\u{00B3}' | '\u{00B9}' |
        '\u{2070}'..='\u{209F}' |
        '\u{00A2}'..='\u{00A5}' |
        '\u{20AC}' |
        '\u{00A9}' | '\u{00AE}' | '\u{2122}' |
        '\u{2500}'..='\u{257F}' |
        '\u{2580}'..='\u{259F}' |
        // Emoji and pictographs
        '\u{2700}'..='\u{27BF}' |
        '\u{FE00}'..='\u{FE0F}' |
        '\u{1F000}'..='\u{1FAFF}'
    )
}
