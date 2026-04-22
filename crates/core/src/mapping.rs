use crate::config::model::{FlowConfig, MappingEntry};
use crate::error::ConfigError;

/// Column-level mapping derived from the flow config.
/// Only `from → to` is supported in MVP — the loader rejects transforms earlier.
#[derive(Debug, Clone)]
pub struct ColumnMapping {
    pub from: String,
    pub to: String,
}

pub fn build(flow: &FlowConfig) -> Result<Vec<ColumnMapping>, ConfigError> {
    if flow.mapping.is_empty() {
        return Err(ConfigError::Invalid {
            reason: "mapping is empty — at least one column mapping is required".into(),
        });
    }

    flow.mapping
        .iter()
        .map(|entry| match entry {
            MappingEntry::Simple(m) => Ok(ColumnMapping {
                from: m.from.clone(),
                to: m.to.clone(),
            }),
            MappingEntry::Object(obj) => {
                if obj.from.transform.is_some() || obj.from.timezone.is_some() {
                    return Err(ConfigError::UnsupportedInMvp {
                        what: format!("transform/timezone on field {:?}", obj.from.name),
                    });
                }
                Ok(ColumnMapping {
                    from: obj.from.name.clone(),
                    to: obj.to.clone(),
                })
            }
        })
        .collect()
}

/// Source column names preserving mapping order.
pub fn source_columns(mappings: &[ColumnMapping]) -> Vec<String> {
    mappings.iter().map(|m| m.from.clone()).collect()
}

pub fn sink_columns(mappings: &[ColumnMapping]) -> Vec<String> {
    mappings.iter().map(|m| m.to.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{CursorConfig, CursorOrder, FlowConfig, SimpleMapping};

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
                interval: "1s".into(),
            },
            batch_limit: 100,
            operation_timeout_secs: None,
        }
    }

    #[test]
    fn empty_mapping_rejected() {
        let err = build(&flow_with_mappings(vec![])).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
    }

    #[test]
    fn simple_mappings_pass_through() {
        let mappings = build(&flow_with_mappings(vec![MappingEntry::Simple(
            SimpleMapping {
                from: "a".into(),
                to: "b".into(),
            },
        )]))
        .unwrap();
        assert_eq!(source_columns(&mappings), vec!["a".to_string()]);
        assert_eq!(sink_columns(&mappings), vec!["b".to_string()]);
    }
}
