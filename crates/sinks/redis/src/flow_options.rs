//! Per-flow redis options and the per-mode column contract.
//!
//! The sink's write mode is declared per flow via the developed sink
//! form `sink = { name = "...", mode = "..." }`. The type matrix checks
//! each column's type and nullability (the sink reports the precise
//! per-mode schema), but it can't express *which* columns a mode
//! requires — so the per-mode contract (required / optional column set)
//! is enforced here instead.
//!
//! Columns are resolved by **name**: each mode reads a fixed set of
//! mapped sink-column names (`key`, `value`, `ttl`). The positional
//! contract `spec.columns[i] == row.values[i]` then lets `write_batch`
//! read each value straight off the row by the index resolved here.

use serde::Deserialize;

use air_elt_core::error::ConfigError;

/// Mapped sink-column name carrying the redis key suffix.
pub const COL_KEY: &str = "key";
/// Mapped sink-column name carrying the JSON payload.
pub const COL_VALUE: &str = "value";
/// Mapped sink-column name carrying the optional TTL (`Interval`).
pub const COL_TTL: &str = "ttl";

/// Redis write mode. One per flow, fixes which command `write_batch`
/// issues and which mapped columns the flow must declare. Defaults to
/// `kv` when the flow omits `mode` on the developed sink form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedisMode {
    /// `SET {to}{key} {json} [PX ttl]`.
    #[default]
    Kv,
    /// `DEL {to}{key}`.
    KvDelete,
    /// `RPUSH {to}{key?} {json}`.
    List,
    /// `XADD {to}{key} * data {json}`.
    Stream,
    /// `PUBLISH {to}{key?} {json}`.
    Pubsub,
}

impl RedisMode {
    /// Stable lower-case tag used in error messages and logs.
    pub fn as_str(self) -> &'static str {
        match self {
            RedisMode::Kv => "kv",
            RedisMode::KvDelete => "kv-delete",
            RedisMode::List => "list",
            RedisMode::Stream => "stream",
            RedisMode::Pubsub => "pubsub",
        }
    }

    /// `(required, optional)` mapped-column names for this mode. Every
    /// required column must be present; every declared column must be a
    /// member of `required ∪ optional` — no others are accepted.
    fn column_contract(self) -> (&'static [&'static str], &'static [&'static str]) {
        match self {
            RedisMode::Kv => (&[COL_KEY, COL_VALUE], &[COL_TTL]),
            RedisMode::KvDelete => (&[COL_KEY], &[]),
            RedisMode::List => (&[COL_VALUE], &[COL_KEY]),
            RedisMode::Stream => (&[COL_KEY, COL_VALUE], &[]),
            RedisMode::Pubsub => (&[COL_VALUE], &[COL_KEY]),
        }
    }

    /// Resolve `spec.columns` into a [`ColumnLayout`], rejecting a
    /// mapping that does not satisfy this mode's column contract.
    pub fn resolve_layout(self, columns: &[String]) -> Result<ColumnLayout, ConfigError> {
        let (required, optional) = self.column_contract();

        for col in columns {
            let known = required.contains(&col.as_str()) || optional.contains(&col.as_str());
            if !known {
                return Err(ConfigError::Invalid {
                    reason: format!(
                        "redis sink mode {:?}: unexpected mapped column {col:?}; \
                         this mode accepts only {}",
                        self.as_str(),
                        describe_allowed(required, optional),
                    ),
                });
            }
        }

        for req in required {
            if !columns.iter().any(|c| c == req) {
                return Err(ConfigError::Invalid {
                    reason: format!(
                        "redis sink mode {:?} requires a mapped sink column named {req:?}",
                        self.as_str(),
                    ),
                });
            }
        }

        Ok(ColumnLayout {
            key_idx: position(columns, COL_KEY),
            value_idx: position(columns, COL_VALUE),
            ttl_idx: position(columns, COL_TTL),
        })
    }
}

/// Resolved positional indices of the mode's columns into `row.values`.
/// `None` means the (optional) column is absent for this flow.
#[derive(Debug, Clone, Copy)]
pub struct ColumnLayout {
    pub key_idx: Option<usize>,
    pub value_idx: Option<usize>,
    pub ttl_idx: Option<usize>,
}

/// Per-flow options deserialized from the developed sink form's table
/// (`sink = { name = "...", mode = "..." }`). `deny_unknown_fields` is
/// safe: `FlowSinkRef::Detailed` already strips `name` out of the
/// options table before it reaches here.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RedisFlowOptions {
    /// Write mode. Defaults to `kv` when omitted (bare `sink = "redis"`
    /// or developed form without `mode`).
    #[serde(default)]
    pub mode: RedisMode,
}

fn position(columns: &[String], name: &str) -> Option<usize> {
    columns.iter().position(|c| c == name)
}

fn describe_allowed(required: &[&str], optional: &[&str]) -> String {
    let mut parts: Vec<String> = required
        .iter()
        .map(|c| format!("{c:?} (required)"))
        .collect();
    parts.extend(optional.iter().map(|c| format!("{c:?} (optional)")));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn flow_options_parse_kebab_mode() {
        let table: toml::Table = toml::from_str(r#"mode = "kv-delete""#).expect("toml");
        let opts: RedisFlowOptions = table.try_into().expect("parse");
        assert_eq!(opts.mode, RedisMode::KvDelete);
    }

    #[test]
    fn flow_options_empty_defaults_to_kv() {
        // Bare `sink = "redis"` yields an empty options table; mode
        // defaults to kv.
        let table = toml::Table::new();
        let opts: RedisFlowOptions = table.try_into().expect("parse");
        assert_eq!(opts.mode, RedisMode::Kv);
    }

    #[test]
    fn flow_options_reject_unknown_field() {
        let table: toml::Table = toml::from_str("mode = \"kv\"\nbogus = 1").expect("toml");
        let err = table.try_into::<RedisFlowOptions>();
        assert!(err.is_err(), "deny_unknown_fields must reject `bogus`");
    }

    #[test]
    fn kv_layout_resolves_all_three() {
        let layout = RedisMode::Kv
            .resolve_layout(&cols(&["key", "value", "ttl"]))
            .expect("ok");
        assert_eq!(layout.key_idx, Some(0));
        assert_eq!(layout.value_idx, Some(1));
        assert_eq!(layout.ttl_idx, Some(2));
    }

    #[test]
    fn kv_layout_ttl_optional() {
        let layout = RedisMode::Kv
            .resolve_layout(&cols(&["key", "value"]))
            .expect("ok");
        assert_eq!(layout.ttl_idx, None);
    }

    #[test]
    fn kv_layout_rejects_missing_value() {
        let err = RedisMode::Kv
            .resolve_layout(&cols(&["key"]))
            .expect_err("missing value");
        match err {
            ConfigError::Invalid { reason } => {
                assert!(reason.contains("value"), "{reason}");
                assert!(reason.contains("kv"), "{reason}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn kv_layout_rejects_extra_column() {
        // `ttl` is fine for kv, but `channel` is not a kv column.
        let err = RedisMode::Kv
            .resolve_layout(&cols(&["key", "value", "channel"]))
            .expect_err("extra column");
        match err {
            ConfigError::Invalid { reason } => assert!(reason.contains("channel"), "{reason}"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn list_key_is_optional() {
        let with = RedisMode::List
            .resolve_layout(&cols(&["value", "key"]))
            .expect("ok");
        assert_eq!(with.key_idx, Some(1));
        let without = RedisMode::List
            .resolve_layout(&cols(&["value"]))
            .expect("ok");
        assert_eq!(without.key_idx, None);
    }

    #[test]
    fn list_rejects_ttl() {
        // `ttl` is only meaningful for kv; list must reject it.
        let err = RedisMode::List
            .resolve_layout(&cols(&["value", "ttl"]))
            .expect_err("ttl not allowed");
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn kv_delete_requires_only_key() {
        let layout = RedisMode::KvDelete
            .resolve_layout(&cols(&["key"]))
            .expect("ok");
        assert_eq!(layout.key_idx, Some(0));
        assert_eq!(layout.value_idx, None);
        let err = RedisMode::KvDelete
            .resolve_layout(&cols(&["key", "value"]))
            .expect_err("value not allowed");
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn stream_requires_key_and_value() {
        RedisMode::Stream
            .resolve_layout(&cols(&["key", "value"]))
            .expect("ok");
        RedisMode::Stream
            .resolve_layout(&cols(&["value"]))
            .expect_err("stream needs key");
    }

    #[test]
    fn pubsub_key_optional() {
        RedisMode::Pubsub
            .resolve_layout(&cols(&["value"]))
            .expect("ok");
        RedisMode::Pubsub
            .resolve_layout(&cols(&["value", "key"]))
            .expect("ok");
    }
}
