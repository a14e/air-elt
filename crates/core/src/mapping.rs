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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
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
        let mappings = build(&flow_with_mappings(vec![MappingEntry::Simple(
            SimpleMapping {
                from: "a".into(),
                to: "b".into(),
            },
        )]))
        .unwrap();
        assert_eq!(mappings[0].from, "a");
        assert_eq!(mappings[0].to, "b");
    }
}
