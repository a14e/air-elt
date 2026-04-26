use crate::config::model::{FlowConfig, MappingEntry};
use crate::error::ConfigError;

/// Column-level mapping derived from the flow config.
///
/// `truncate` opts the column into narrowing conversions. `default_literal`
/// is the raw TOML literal supplied by the operator — it cannot be parsed
/// into a typed `Value` here because the sink's `DataType` is unknown until
/// schema introspection runs in `validation::pipeline::validate`. That
/// stage reads `default_literal`, parses it against the resolved sink
/// `DataType`, and stores the typed result on the corresponding
/// `ConversionPlan::ctx.default`. Past validation the literal is no longer
/// needed.
#[derive(Debug, Clone)]
pub struct ColumnMapping {
    pub from: String,
    pub to: String,
    pub truncate: bool,
    pub default_literal: Option<toml::Value>,
}

pub fn build(flow: &FlowConfig) -> Result<Vec<ColumnMapping>, ConfigError> {
    if flow.mapping.is_empty() {
        return Err(ConfigError::Invalid {
            reason: "mapping is empty — at least one column mapping is required".into(),
        });
    }

    flow.mapping
        .iter()
        .map(|entry: &MappingEntry| {
            Ok(ColumnMapping {
                from: entry.from.clone(),
                to: entry.to.clone(),
                truncate: entry.truncate,
                default_literal: entry.default.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::model::{CursorConfig, CursorOrder, FlowConfig};

    fn flow_with_mappings(entries: Vec<MappingEntry>) -> FlowConfig {
        FlowConfig {
            source: "s".into(),
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
        }
    }

    #[test]
    fn empty_mapping_rejected() {
        let err = build(&flow_with_mappings(vec![])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn simple_mappings_pass_through() {
        let mappings = build(&flow_with_mappings(vec![MappingEntry {
            from: "a".into(),
            to: "b".into(),
            truncate: false,
            default: None,
        }]))
        .unwrap();
        assert_eq!(mappings[0].from, "a");
        assert_eq!(mappings[0].to, "b");
        assert!(!mappings[0].truncate);
        assert!(mappings[0].default_literal.is_none());
    }
}
