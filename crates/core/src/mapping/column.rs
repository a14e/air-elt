//! Normalised mapping rules.
//!
//! [`build`] consumes a flow's raw `mapping = [...]` array (after the
//! loader resolves shorthand vs full forms via
//! [`MappingRule`](crate::config::model::MappingRule)) and produces an
//! ordered [`Vec<ColumnMapping>`]. The normaliser rejects only
//! structural problems whose verdict does not depend on schema
//! introspection — empty mapping, multiple wildcards, wildcard
//! mixed with json-pack, and duplicate short-form identities.
//! Multiple JSON auto-pack entries are allowed as long as their
//! target columns differ; the post-expansion sink-uniqueness check
//! catches duplicates. Schema-aware checks (matrix compatibility,
//! cursor / conflict subset, sink-column uniqueness post-expansion)
//! happen later in the validation pipeline (`mapping::expand` →
//! `validation::checks`).

use crate::config::model::{FlowConfig, MappingEntry};
use crate::error::ConfigError;
use crate::mapping::shorthand::{ParsedShorthand, parse as parse_shorthand};

/// One mapping rule, post-shorthand resolution but pre-schema
/// expansion. Used by `validation::pipeline::validate` to drive
/// wildcard fan-out and JSON auto-pack planning.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnMapping {
    /// A concrete `from → to` column mapping. `truncate` and
    /// `default_literal` are reachable only via the long-form table
    /// (the shorthand grammar omits them by design).
    Direct {
        from: String,
        to: String,
        truncate: bool,
        default_literal: Option<toml::Value>,
    },
    /// A wildcard `"*"` rule. Resolved at validation time against the
    /// sink schema (preferred) or source schema; falls back to a
    /// raw-passthrough plan when both sides are schemaless.
    Wildcard,
    /// A JSON auto-pack rule (`"*:NAME"`). Every source field is
    /// folded into one `Value::Json` placed in sink column `to`.
    Body { to: String },
}

/// Normalise a flow's `mapping = [...]` array.
///
/// Rejects the structural problems listed in the module docs.
/// Schema-dependent checks remain the responsibility of
/// `validation::pipeline::validate` (running after schema
/// introspection and `mapping::expand`).
pub fn build(flow: &FlowConfig) -> Result<Vec<ColumnMapping>, ConfigError> {
    if flow.mapping.is_empty() {
        return Err(ConfigError::Invalid {
            reason: "mapping is empty — at least one column mapping is required".into(),
        });
    }

    let mut out: Vec<ColumnMapping> = Vec::with_capacity(flow.mapping.len());
    let mut wildcard_seen = false;
    let mut body_seen = false;

    for rule in &flow.mapping {
        let normalised = normalize_rule(rule)?;
        match &normalised {
            ColumnMapping::Wildcard => {
                if wildcard_seen {
                    return Err(ConfigError::Invalid {
                        reason: "mapping has more than one wildcard ('*'/'*:*') rule".into(),
                    });
                }
                if body_seen {
                    return Err(ConfigError::Invalid {
                        reason: "mapping mixes wildcard ('*') with JSON auto-pack ('*:NAME') — \
                                 they are mutually exclusive"
                            .into(),
                    });
                }
                wildcard_seen = true;
            }
            ColumnMapping::Body { .. } => {
                if wildcard_seen {
                    return Err(ConfigError::Invalid {
                        reason: "mapping mixes wildcard ('*') with JSON auto-pack ('*:NAME') — \
                                 they are mutually exclusive"
                            .into(),
                    });
                }
                // Multiple Body entries are allowed as long as their
                // `to`-names differ — duplicate sink targets are caught
                // by `check_sink_uniqueness` after expansion.
                body_seen = true;
            }
            ColumnMapping::Direct { .. } => {}
        }
        out.push(normalised);
    }

    // Reject duplicate short-form identities (parsed equality of the
    // (from, to) pair) — long-form duplicates surface in the schema-
    // aware uniqueness check post-expansion.
    check_short_form_dups(&flow.mapping, &out)?;

    Ok(out)
}

/// Normalise a single rule. Long-form passes through as `Direct`;
/// shorthand strings are parsed via [`shorthand::parse`].
fn normalize_rule(rule: &crate::config::model::MappingRule) -> Result<ColumnMapping, ConfigError> {
    match rule {
        crate::config::model::MappingRule::Full(entry) => Ok(direct_from_entry(entry)),
        crate::config::model::MappingRule::Shorthand(s) => match parse_shorthand(s)? {
            ParsedShorthand::Renamed { from, to } => Ok(ColumnMapping::Direct {
                from,
                to,
                truncate: false,
                default_literal: None,
            }),
            ParsedShorthand::Wildcard => Ok(ColumnMapping::Wildcard),
            ParsedShorthand::Body { to } => Ok(ColumnMapping::Body { to }),
        },
    }
}

fn direct_from_entry(entry: &MappingEntry) -> ColumnMapping {
    ColumnMapping::Direct {
        from: entry.from.clone(),
        to: entry.to.clone(),
        truncate: entry.truncate,
        default_literal: entry.default.clone(),
    }
}

/// Reject two short-form rules that resolve to the same (from, to)
/// pair. We do this only for rules that arrived through the shorthand
/// surface so the equivalent long-form entries (which may legitimately
/// repeat with different `truncate` / `default` knobs in some edge
/// configurations) keep flowing through to schema-aware uniqueness
/// checks downstream.
fn check_short_form_dups(
    raw: &[crate::config::model::MappingRule],
    normalised: &[ColumnMapping],
) -> Result<(), ConfigError> {
    let mut seen: ahash::AHashSet<(&str, &str)> = ahash::AHashSet::new();
    for (i, rule) in raw.iter().enumerate() {
        if !matches!(rule, crate::config::model::MappingRule::Shorthand(_)) {
            continue;
        }
        let ColumnMapping::Direct { from, to, .. } = &normalised[i] else {
            continue;
        };
        let pair = (from.as_str(), to.as_str());
        if !seen.insert(pair) {
            return Err(ConfigError::Invalid {
                reason: format!(
                    "shorthand mapping rule {:?} duplicates an earlier short-form entry",
                    short_input(&raw[i]),
                ),
            });
        }
    }
    Ok(())
}

fn short_input(rule: &crate::config::model::MappingRule) -> &str {
    match rule {
        crate::config::model::MappingRule::Shorthand(s) => s.as_str(),
        crate::config::model::MappingRule::Full(_) => "<long-form>",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::model::{
        CursorConfig, CursorOrder, FlowConfig, FlowSourceRef, MappingEntry, MappingRule,
    };

    fn flow_with_mappings(entries: Vec<MappingRule>) -> FlowConfig {
        FlowConfig {
            source: FlowSourceRef::Bare("s".into()),
            sink: "k".into(),
            storage: "st".into(),
            from: "t".into(),
            to: "t".into(),
            mapping: entries,
            cursor: CursorConfig {
                fields: vec!["id".into()],
                order: CursorOrder::Asc,
                interval: std::time::Duration::from_secs(1),
            },
            batch_limit: 100,
            query_timeout: None,
            validation: Default::default(),
            conflict: None,
        }
    }

    fn full(from: &str, to: &str) -> MappingRule {
        MappingRule::Full(MappingEntry {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default: None,
        })
    }

    fn short(s: &str) -> MappingRule {
        MappingRule::Shorthand(s.into())
    }

    #[test]
    fn empty_mapping_rejected() {
        let err = build(&flow_with_mappings(vec![])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn long_form_pass_through() {
        let rules = build(&flow_with_mappings(vec![full("a", "b")])).unwrap();
        assert_eq!(rules.len(), 1);
        assert!(matches!(
            &rules[0],
            ColumnMapping::Direct { from, to, .. } if from == "a" && to == "b"
        ));
    }

    #[test]
    fn shorthand_identity_normalises_to_direct() {
        let rules = build(&flow_with_mappings(vec![short("id")])).unwrap();
        assert_eq!(
            rules,
            vec![ColumnMapping::Direct {
                from: "id".into(),
                to: "id".into(),
                truncate: false,
                default_literal: None,
            }]
        );
    }

    #[test]
    fn shorthand_rename_normalises_to_direct() {
        let rules = build(&flow_with_mappings(vec![short("a:b")])).unwrap();
        assert_eq!(
            rules,
            vec![ColumnMapping::Direct {
                from: "a".into(),
                to: "b".into(),
                truncate: false,
                default_literal: None,
            }]
        );
    }

    #[test]
    fn shorthand_wildcard_and_json_pack() {
        let rules = build(&flow_with_mappings(vec![short("*"), short("*:body")])).unwrap_err();
        // Both together is rejected by `build`.
        assert!(matches!(rules, ConfigError::Invalid { .. }));
    }

    /// Multiple wildcards rejected.
    #[test]
    fn multiple_wildcards_rejected() {
        let err = build(&flow_with_mappings(vec![short("*"), short("*")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    /// Wildcard + json-pack rejected.
    #[test]
    fn wildcard_and_json_pack_rejected() {
        let err = build(&flow_with_mappings(vec![short("*"), short("*:body")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
        let err = build(&flow_with_mappings(vec![short("*:body"), short("*")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    /// Multiple JSON auto-pack rules with **distinct** sink targets are
    /// admitted — each pack adds one synthetic sink column and the
    /// post-expansion sink-uniqueness check enforces non-collision.
    /// Same target twice is rejected later as `DuplicateSinkField`.
    #[test]
    fn multiple_body_allowed_with_distinct_targets() {
        let rules = build(&flow_with_mappings(vec![short("*:a"), short("*:b")])).unwrap();
        assert_eq!(
            rules,
            vec![
                ColumnMapping::Body { to: "a".into() },
                ColumnMapping::Body { to: "b".into() },
            ]
        );
    }

    /// Mixed `[long, "*", "id"]` round-trips through `build` without
    /// expansion. We assert the rule kinds and order.
    #[test]
    fn mixed_round_trips_through_build() {
        let rules = build(&flow_with_mappings(vec![
            full("a", "b"),
            short("*"),
            short("id"),
        ]))
        .unwrap();
        assert_eq!(rules.len(), 3);
        assert!(matches!(
            &rules[0],
            ColumnMapping::Direct { from, to, .. } if from == "a" && to == "b"
        ));
        assert_eq!(rules[1], ColumnMapping::Wildcard);
        assert!(matches!(
            &rules[2],
            ColumnMapping::Direct { from, to, .. } if from == "id" && to == "id"
        ));
    }

    /// Checked-in fixture: a long-form-only mapping keeps producing the
    /// same ordered direct rules.
    #[test]
    fn long_form_only_back_compat_fixture() {
        let rules = build(&flow_with_mappings(vec![
            full("id", "id"),
            full("name", "name"),
        ]))
        .unwrap();
        assert_eq!(
            rules,
            vec![
                ColumnMapping::Direct {
                    from: "id".into(),
                    to: "id".into(),
                    truncate: false,
                    default_literal: None,
                },
                ColumnMapping::Direct {
                    from: "name".into(),
                    to: "name".into(),
                    truncate: false,
                    default_literal: None,
                },
            ]
        );
    }

    /// Two identical short-form rules collide. (Long-form duplicates
    /// continue to flow through to the schema-aware sink-uniqueness
    /// check.)
    #[test]
    fn duplicate_short_form_rejected() {
        let err = build(&flow_with_mappings(vec![short("id"), short("id")])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }
}
