//! Normalised mapping rules.
//!
//! [`build`] consumes a flow's raw [`MappingMap`](crate::config::model::MappingMap)
//! (TOML/YAML object keyed by sink column) and produces an ordered
//! [`Vec<ColumnMapping>`]. The normaliser rejects only structural
//! problems whose verdict does not depend on schema introspection —
//! empty mapping, multiple wildcards, wildcard mixed with body-pack,
//! and unparseable special markers. Schema-aware checks (matrix
//! compatibility, cursor / conflict subset, sink-column uniqueness
//! post-expansion) happen later in the validation pipeline
//! (`mapping::expand` → `validation::checks`).

use crate::config::model::{FlowConfig, MappingEntry, MappingRhs, SwitchTable};
use crate::error::ConfigError;

/// Reserved RHS marker that triggers either wildcard fan-out (when the
/// LHS key is also `"*"`) or body-pack into the LHS sink column
/// (otherwise).
const WILDCARD_MARKER: &str = "*";

/// One mapping rule, post-config-parse but pre-schema expansion. Used
/// by `validation::pipeline::validate` to drive wildcard fan-out, body
/// auto-pack planning, and switch table lowering.
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnMapping {
    /// A concrete `from → to` column mapping. `truncate` and
    /// `default_literal` come from the long-form entry; the short
    /// `key = "value"` form sets both to defaults.
    Direct {
        from: String,
        to: String,
        truncate: bool,
        default_literal: Option<toml::Value>,
    },
    /// A wildcard `"*" = "*"` rule. Resolved at validation time against
    /// the sink schema (preferred) or source schema; falls back to a
    /// raw-passthrough plan when both sides are schemaless.
    Wildcard,
    /// A body auto-pack rule (`NAME = "*"`). Every source field is
    /// folded into one body payload placed in sink column `to`.
    Body { to: String },
    /// A value-to-value switch lookup. Lowered to `TransformOp::Switch`
    /// at compile time; key/value canonicalisation happens against the
    /// schemas in the validation pipeline.
    Switch {
        from: String,
        to: String,
        truncate: bool,
        cases: Vec<SwitchCase>,
        default_literal: Option<toml::Value>,
    },
}

/// One arm of a [`ColumnMapping::Switch`] table. `key` is the literal
/// TOML key text (TOML inline-table keys are always strings); both
/// `key` and `value` are canonicalised against the source/sink data
/// types during compile.
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchCase {
    pub key: String,
    pub value: toml::Value,
}

/// Normalise a flow's `[mapping]` table.
///
/// Rejects: empty mapping, multiple wildcards (cannot happen with
/// unique keys but checked defensively), wildcard mixed with body-pack,
/// `"*"` as RHS for a non-`"*"` key conflicting with a `"*"` key,
/// empty/whitespace-only key or RHS, `to` field smuggled inside the
/// long-form entry (rejected by `deny_unknown_fields` upstream).
pub fn build(flow: &FlowConfig) -> Result<Vec<ColumnMapping>, ConfigError> {
    if flow.mapping.is_empty() {
        return Err(ConfigError::Invalid {
            reason: "mapping is empty — at least one column mapping is required".into(),
        });
    }

    let mut out: Vec<ColumnMapping> = Vec::with_capacity(flow.mapping.len());
    let mut wildcard_seen = false;
    let mut body_seen = false;

    for (key, rhs) in flow.mapping.iter() {
        validate_key(key)?;
        let normalised = normalise_entry(key, rhs)?;
        match &normalised {
            ColumnMapping::Wildcard => {
                if wildcard_seen {
                    return Err(ConfigError::Invalid {
                        reason: "mapping has more than one wildcard ('*' = '*') rule".into(),
                    });
                }
                if body_seen {
                    return Err(ConfigError::Invalid {
                        reason: "mapping mixes wildcard ('*' = '*') with body-pack \
                                 (NAME = '*') — they are mutually exclusive"
                            .into(),
                    });
                }
                wildcard_seen = true;
            }
            ColumnMapping::Body { .. } => {
                if wildcard_seen {
                    return Err(ConfigError::Invalid {
                        reason: "mapping mixes wildcard ('*' = '*') with body-pack \
                                 (NAME = '*') — they are mutually exclusive"
                            .into(),
                    });
                }
                body_seen = true;
            }
            ColumnMapping::Direct { .. } | ColumnMapping::Switch { .. } => {}
        }
        out.push(normalised);
    }

    Ok(out)
}

fn validate_key(key: &str) -> Result<(), ConfigError> {
    if key.is_empty() {
        return Err(ConfigError::Invalid {
            reason: "mapping key is empty".into(),
        });
    }
    if key.chars().any(char::is_whitespace) {
        return Err(ConfigError::Invalid {
            reason: format!("mapping key {key:?} contains whitespace"),
        });
    }
    Ok(())
}

fn normalise_entry(key: &str, rhs: &MappingRhs) -> Result<ColumnMapping, ConfigError> {
    match rhs {
        MappingRhs::Short(s) => normalise_short(key, s),
        MappingRhs::Full(entry) => normalise_full(key, entry),
    }
}

fn normalise_short(key: &str, rhs: &str) -> Result<ColumnMapping, ConfigError> {
    if rhs.is_empty() {
        return Err(ConfigError::Invalid {
            reason: format!("mapping value for key {key:?} is empty"),
        });
    }
    if rhs.chars().any(char::is_whitespace) {
        return Err(ConfigError::Invalid {
            reason: format!("mapping value {rhs:?} for key {key:?} contains whitespace"),
        });
    }

    match (key == WILDCARD_MARKER, rhs == WILDCARD_MARKER) {
        // `"*" = "*"` — wildcard fan-out.
        (true, true) => Ok(ColumnMapping::Wildcard),
        // `KEY = "*"` for KEY ≠ "*" — body-pack.
        (false, true) => Ok(ColumnMapping::Body { to: key.into() }),
        // `"*" = "SOMETHING"` for SOMETHING ≠ "*" is meaningless — the
        // wildcard key requires the wildcard value.
        (true, false) => Err(ConfigError::Invalid {
            reason: format!("mapping key \"*\" only accepts the wildcard value \"*\", got {rhs:?}"),
        }),
        // `KEY = "SRC"` — plain `from = SRC, to = KEY`. Identity and
        // rename collapse to the same case.
        (false, false) => Ok(ColumnMapping::Direct {
            from: rhs.into(),
            to: key.into(),
            truncate: false,
            default_literal: None,
        }),
    }
}

fn normalise_full(key: &str, entry: &MappingEntry) -> Result<ColumnMapping, ConfigError> {
    if key == WILDCARD_MARKER {
        return Err(ConfigError::Invalid {
            reason: "mapping key \"*\" must use the short form `\"*\" = \"*\"` and cannot carry \
                     a full entry table"
                .into(),
        });
    }
    if entry.from.is_empty() {
        return Err(ConfigError::Invalid {
            reason: format!("mapping entry for key {key:?} has empty `from`"),
        });
    }
    if entry.from == WILDCARD_MARKER {
        // `KEY = { from = "*", ... }` — body-pack with optional
        // `truncate` / `default` / `switch`. Switch on a body is
        // forbidden — there is no scalar value to dispatch on. The
        // other knobs (truncate, default) are also nonsensical at this
        // level today; we keep the same rule the shorthand path
        // implied — body-pack is only valid via the bare `KEY = "*"`
        // form.
        return Err(ConfigError::Invalid {
            reason: format!(
                "mapping entry for key {key:?} has `from = \"*\"` — body-pack must use the \
                 short form `{key} = \"*\"` and cannot mix with `truncate`/`default`/`switch`"
            ),
        });
    }

    if let Some(switch) = &entry.switch {
        if switch.is_empty() {
            return Err(ConfigError::Invalid {
                reason: format!("mapping entry for key {key:?} has empty `switch` table"),
            });
        }
        return Ok(ColumnMapping::Switch {
            from: entry.from.clone(),
            to: key.into(),
            truncate: entry.truncate,
            cases: switch_cases(switch),
            default_literal: entry.default.clone(),
        });
    }

    Ok(ColumnMapping::Direct {
        from: entry.from.clone(),
        to: key.into(),
        truncate: entry.truncate,
        default_literal: entry.default.clone(),
    })
}

fn switch_cases(switch: &SwitchTable) -> Vec<SwitchCase> {
    switch
        .iter()
        .map(|(k, v)| SwitchCase {
            key: k.clone(),
            value: v.clone(),
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::model::{
        CursorConfig, CursorOrder, FlowConfig, FlowSourceRef, MappingEntry, MappingMap, MappingRhs,
        SwitchTable,
    };

    fn flow_with(entries: Vec<(&str, MappingRhs)>) -> FlowConfig {
        let mapping = MappingMap(entries.into_iter().map(|(k, v)| (k.into(), v)).collect());
        FlowConfig {
            source: FlowSourceRef::Bare("s".into()),
            sink: "k".into(),
            storage: "st".into(),
            from: "t".into(),
            to: "t".into(),
            mapping,
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

    fn full_entry(from: &str) -> MappingRhs {
        MappingRhs::Full(MappingEntry {
            from: from.into(),
            truncate: false,
            default: None,
            switch: None,
        })
    }

    fn short(s: &str) -> MappingRhs {
        MappingRhs::Short(s.into())
    }

    #[test]
    fn empty_mapping_rejected() {
        let flow = flow_with(vec![]);
        let err = build(&flow).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn identity_short_form() {
        let rules = build(&flow_with(vec![("id", short("id"))])).unwrap();
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
    fn rename_short_form() {
        let rules = build(&flow_with(vec![("dst", short("src"))])).unwrap();
        assert_eq!(
            rules,
            vec![ColumnMapping::Direct {
                from: "src".into(),
                to: "dst".into(),
                truncate: false,
                default_literal: None,
            }]
        );
    }

    #[test]
    fn wildcard_pair() {
        let rules = build(&flow_with(vec![("*", short("*"))])).unwrap();
        assert_eq!(rules, vec![ColumnMapping::Wildcard]);
    }

    #[test]
    fn wildcard_with_non_wildcard_value_rejected() {
        let err = build(&flow_with(vec![("*", short("name"))])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn body_pack_short_form() {
        let rules = build(&flow_with(vec![("body", short("*"))])).unwrap();
        assert_eq!(rules, vec![ColumnMapping::Body { to: "body".into() }]);
    }

    #[test]
    fn wildcard_with_full_entry_rejected() {
        let err = build(&flow_with(vec![("*", full_entry("anything"))])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn long_form_pass_through() {
        let rules = build(&flow_with(vec![("b", full_entry("a"))])).unwrap();
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
    fn long_form_with_truncate_and_default() {
        let rhs = MappingRhs::Full(MappingEntry {
            from: "src".into(),
            truncate: true,
            default: Some(toml::Value::Integer(5)),
            switch: None,
        });
        let rules = build(&flow_with(vec![("dst", rhs)])).unwrap();
        assert_eq!(
            rules,
            vec![ColumnMapping::Direct {
                from: "src".into(),
                to: "dst".into(),
                truncate: true,
                default_literal: Some(toml::Value::Integer(5)),
            }]
        );
    }

    #[test]
    fn switch_form() {
        let switch = SwitchTable(vec![
            ("ACTIVE".into(), toml::Value::String("active".into())),
            ("FINISHED".into(), toml::Value::String("finished".into())),
        ]);
        let rhs = MappingRhs::Full(MappingEntry {
            from: "status".into(),
            truncate: false,
            default: Some(toml::Value::String("unknown".into())),
            switch: Some(switch),
        });
        let rules = build(&flow_with(vec![("status_label", rhs)])).unwrap();
        assert_eq!(rules.len(), 1);
        let ColumnMapping::Switch {
            from,
            to,
            truncate,
            cases,
            default_literal,
        } = &rules[0]
        else {
            panic!("expected Switch, got {:?}", rules[0]);
        };
        assert_eq!(from, "status");
        assert_eq!(to, "status_label");
        assert!(!truncate);
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].key, "ACTIVE");
        assert_eq!(cases[1].key, "FINISHED");
        assert_eq!(
            *default_literal,
            Some(toml::Value::String("unknown".into()))
        );
    }

    #[test]
    fn empty_switch_rejected() {
        let rhs = MappingRhs::Full(MappingEntry {
            from: "status".into(),
            truncate: false,
            default: None,
            switch: Some(SwitchTable(vec![])),
        });
        let err = build(&flow_with(vec![("status_label", rhs)])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn empty_key_rejected() {
        let err = build(&flow_with(vec![("", short("id"))])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn empty_value_rejected() {
        let err = build(&flow_with(vec![("id", short(""))])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn wildcard_and_body_pack_rejected() {
        let err = build(&flow_with(vec![("*", short("*")), ("body", short("*"))])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn multiple_body_pack_allowed_with_distinct_keys() {
        let rules = build(&flow_with(vec![
            ("body", short("*")),
            ("archive", short("*")),
        ]))
        .unwrap();
        assert_eq!(
            rules,
            vec![
                ColumnMapping::Body { to: "body".into() },
                ColumnMapping::Body {
                    to: "archive".into(),
                },
            ]
        );
    }

    #[test]
    fn mixed_wildcard_and_direct_preserves_order() {
        let rules = build(&flow_with(vec![
            ("b", full_entry("a")),
            ("*", short("*")),
            ("id", short("id")),
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

    #[test]
    fn rejects_old_array_syntax() {
        // Round-trip a raw TOML payload through the parser to confirm
        // the old `mapping = [...]` array form produces a clear error.
        let toml_src = r#"
[[sources]]
name = "s"
type = "postgres"

[[sinks]]
name = "k"
type = "postgres"

[[storages]]
name = "st"
type = "postgres"

[flow.f]
source = "s"
sink = "k"
storage = "st"
from = "t"
to = "t"
mapping = [{ from = "id", to = "id" }]
cursor = { fields = ["id"] }
"#;
        let err = toml::from_str::<crate::config::model::RootConfig>(toml_src).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("mapping") && msg.contains("table"),
            "old array form should error with `mapping must now be a table`; got: {msg}"
        );
    }
}
