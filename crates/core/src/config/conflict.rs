//! `[flow.<name>.conflict]` block. Operator-declared upsert behaviour
//! that replaces the older sink-local `upsert_key` knob. Without this
//! block the runner does plain `INSERT` / `insertMany`; with it, the
//! sink performs an upsert keyed on `key` using the chosen `strategy`.
//!
//! The block is sink-agnostic — the SQL backends translate it into
//! `ON CONFLICT (...) DO {NOTHING|UPDATE}` (Postgres) or
//! `INSERT IGNORE` / `ON DUPLICATE KEY UPDATE` (MySQL/MariaDB). Mongo
//! translates `overwrite` to `replaceOne(upsert=true)` and `ignore` to
//! `insertMany(ordered=false)` swallowing E11000 duplicate-key errors.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ConflictStrategy {
    /// Drop the new row when an existing row's key matches.
    Ignore,
    /// Replace the existing row with the new one.
    Overwrite,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConflictConfig {
    /// Sink columns / dot-paths that form the conflict key. Empty
    /// rejected at parse time.
    pub key: Vec<String>,
    pub strategy: ConflictStrategy,
}

impl ConflictConfig {
    /// Validate at deserialization-finalisation time. Called from the
    /// loader so a misconfigured conflict block surfaces as a config
    /// error rather than at validate-time.
    pub fn validate(&self) -> Result<(), String> {
        if self.key.is_empty() {
            return Err("conflict.key must contain at least one field".into());
        }
        for k in &self.key {
            if k.trim().is_empty() {
                return Err("conflict.key entries must be non-empty".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Wrap {
        conflict: ConflictConfig,
    }

    #[test]
    fn parses_overwrite() {
        let cfg: Wrap = toml::from_str(
            r#"
            [conflict]
            key = ["id"]
            strategy = "overwrite"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.conflict.key, vec!["id".to_string()]);
        assert_eq!(cfg.conflict.strategy, ConflictStrategy::Overwrite);
    }

    #[test]
    fn parses_ignore_multikey() {
        let cfg: Wrap = toml::from_str(
            r#"
            [conflict]
            key = ["a", "b"]
            strategy = "ignore"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.conflict.strategy, ConflictStrategy::Ignore);
    }

    #[test]
    fn empty_key_rejected_by_validate() {
        let cfg = ConflictConfig {
            key: vec![],
            strategy: ConflictStrategy::Overwrite,
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn unknown_field_rejected() {
        let r: Result<Wrap, _> = toml::from_str(
            r#"
            [conflict]
            key = ["id"]
            strategy = "overwrite"
            extra = 1
        "#,
        );
        assert!(r.is_err());
    }
}
