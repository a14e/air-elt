use std::fmt;
use std::path::Path;

use crate::workspace::Workspace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

pub struct Finding {
    pub severity: Severity,
    pub file: String,
    pub line: Option<usize>,
    pub message: String,
}

impl Finding {
    pub fn error(file: impl Into<String>, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            file: file.into(),
            line,
            message: message.into(),
        }
    }

    pub fn warning(
        file: impl Into<String>,
        line: Option<usize>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: Severity::Warning,
            file: file.into(),
            line,
            message: message.into(),
        }
    }
}

pub struct Diagnostic {
    pub severity: Severity,
    pub file: String,
    pub line: Option<usize>,
    pub check: &'static str,
    pub message: String,
}

impl Diagnostic {
    pub fn from_finding(finding: Finding, check: &'static str) -> Self {
        Self {
            severity: finding.severity,
            file: finding.file,
            line: finding.line,
            check,
            message: finding.message,
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        match self.line {
            Some(line) => write!(
                f,
                "{severity}: {}:{line}: {} [{}]",
                self.file, self.message, self.check
            ),
            None => write!(
                f,
                "{severity}: {}: {} [{}]",
                self.file, self.message, self.check
            ),
        }
    }
}

pub trait Check {
    fn name(&self) -> &'static str;
    fn run(&self, root: &Path, workspace: &Workspace) -> Vec<Finding>;
}
