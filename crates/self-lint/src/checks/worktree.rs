use std::path::Path;
use std::process::Command;

use crate::diagnostic::{Check, Finding};
use crate::workspace::Workspace;

pub struct WorktreeCheck;

impl Check for WorktreeCheck {
    fn name(&self) -> &'static str {
        "worktree"
    }

    fn run(&self, root: &Path, _workspace: &Workspace) -> Vec<Finding> {
        let mut findings = Vec::new();

        // Check for stale worktree branches
        if let Ok(output) = Command::new("git")
            .args(["branch", "--list", "worktree-agent-*"])
            .current_dir(root)
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                let branch = line.trim().trim_start_matches("* ");
                if !branch.is_empty() {
                    findings.push(Finding::error(
                        "git",
                        None,
                        format!(
                            "stale worktree branch '{branch}' \
                             — delete with `git branch -D {branch}`"
                        ),
                    ));
                }
            }
        }

        // Check for active worktrees (besides main)
        if let Ok(output) = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(root)
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let worktree_count = stdout
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count();
            if worktree_count > 1 {
                findings.push(Finding::error(
                    "git",
                    None,
                    format!(
                        "found {worktree_count} git worktrees (expected 1) \
                         — clean with `git worktree remove`"
                    ),
                ));
            }
        }

        // Check for .claude/worktrees/ directory with content
        let worktrees_dir = root.join(".claude/worktrees");
        if worktrees_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&worktrees_dir) {
                let dirs: Vec<_> = entries
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .collect();
                if !dirs.is_empty() {
                    findings.push(Finding::error(
                        ".claude/worktrees/",
                        None,
                        format!(
                            "found {} stale worktree directories — remove them",
                            dirs.len()
                        ),
                    ));
                }
            }
        }

        findings
    }
}
