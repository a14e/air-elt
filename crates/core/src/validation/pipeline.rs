use std::sync::Arc;

use tracing::{info, instrument};

use crate::config::model::RootConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping::{self};
use crate::registry::Registry;
use crate::traits::{ReadSpec, Sink, Source, Storage, WriteSpec};
use crate::validation::checks;

/// The bundle of resolved components + compiled artifacts for a single flow.
pub struct ResolvedFlow {
    pub name: String,
    pub source: Arc<dyn Source>,
    pub sink: Arc<dyn Sink>,
    pub storage: Arc<dyn Storage>,
    pub read_spec: ReadSpec,
    pub write_spec: WriteSpec,
    pub interval: std::time::Duration,
    /// Per-operation timeout applied by the runner around `read_batch`/
    /// `write_batch`/`save_cursor`/`load_cursor` calls. Overridable per flow
    /// via `operation_timeout_secs`.
    pub operation_timeout: std::time::Duration,
}

/// Stepped validator. Each step emits a typed error and logs the outcome.
#[instrument(skip(root, registry), err)]
pub async fn validate(
    root: &RootConfig,
    registry: &Registry,
) -> Result<Vec<ResolvedFlow>, ValidationError> {
    let mut resolved = Vec::with_capacity(root.flow.len());

    for (flow_name, flow) in &root.flow {
        info!(flow = %flow_name, "validating flow");

        let source_cfg = root
            .sources
            .iter()
            .find(|c| c.name == flow.source)
            .ok_or_else(|| ValidationError::UnknownSource(flow.source.clone()))?;
        let sink_cfg = root
            .sinks
            .iter()
            .find(|c| c.name == flow.sink)
            .ok_or_else(|| ValidationError::UnknownSink(flow.sink.clone()))?;
        let storage_cfg = root
            .storages
            .iter()
            .find(|c| c.name == flow.storage)
            .ok_or_else(|| ValidationError::UnknownStorage(flow.storage.clone()))?;

        let source = registry.build_source(source_cfg).await.map_err(|source| {
            ValidationError::AccessFailed {
                component: "source",
                name: source_cfg.name.clone(),
                source,
            }
        })?;
        let sink = registry.build_sink(sink_cfg).await.map_err(|source| {
            ValidationError::AccessFailed {
                component: "sink",
                name: sink_cfg.name.clone(),
                source,
            }
        })?;
        let storage = registry
            .build_storage(storage_cfg)
            .await
            .map_err(|source| ValidationError::AccessFailed {
                component: "storage",
                name: storage_cfg.name.clone(),
                source,
            })?;

        let mappings = mapping::build(flow).map_err(|e| ValidationError::AccessFailed {
            component: "mapping",
            name: flow_name.clone(),
            source: RuntimeError::Other(e.to_string()),
        })?;

        let read_spec = ReadSpec {
            columns: mapping::source_columns(&mappings),
            table: flow.from.clone(),
            cursor_fields: flow.cursor.fields.clone(),
            cursor_order: flow.cursor.order,
            limit: flow.batch_limit,
        };
        let write_spec = WriteSpec {
            columns: mapping::sink_columns(&mappings),
            table: flow.to.clone(),
        };

        // Access checks — actual DB connections, ping SELECT 1 + insert-where-false etc.
        storage
            .validate_access()
            .await
            .map_err(|source| ValidationError::AccessFailed {
                component: "storage",
                name: storage_cfg.name.clone(),
                source,
            })?;
        source.validate_access(&read_spec).await.map_err(|source| {
            ValidationError::AccessFailed {
                component: "source",
                name: source_cfg.name.clone(),
                source,
            }
        })?;
        sink.validate_access(&write_spec).await.map_err(|source| {
            ValidationError::AccessFailed {
                component: "sink",
                name: sink_cfg.name.clone(),
                source,
            }
        })?;

        // Schemas + type matrix.
        let src_schema = source.describe_schema(&flow.from).await.map_err(|source| {
            ValidationError::AccessFailed {
                component: "source:schema",
                name: source_cfg.name.clone(),
                source,
            }
        })?;
        let dst_schema = sink.describe_schema(&flow.to).await.map_err(|source| {
            ValidationError::AccessFailed {
                component: "sink:schema",
                name: sink_cfg.name.clone(),
                source,
            }
        })?;

        checks::check_cursor(flow_name, &src_schema, &flow.cursor.fields)?;
        checks::check_mapping(&src_schema, &dst_schema, &mappings)?;

        let interval =
            parse_interval(&flow.cursor.interval).map_err(|e| ValidationError::AccessFailed {
                component: "cursor:interval",
                name: flow_name.clone(),
                source: RuntimeError::Other(e),
            })?;

        // Why: flow-level operation_timeout_secs overrides the workspace default
        // — long-running ELT batches (minutes to hours) need it configurable.
        let operation_timeout = flow
            .operation_timeout_secs
            .map(std::time::Duration::from_secs)
            .unwrap_or_else(|| std::time::Duration::from_secs(30));

        info!(flow = %flow_name, "flow validated");
        resolved.push(ResolvedFlow {
            name: flow_name.clone(),
            source,
            sink,
            storage,
            read_spec,
            write_spec,
            interval,
            operation_timeout,
        });
    }

    Ok(resolved)
}

/// Minimal duration parser for strings like "1s", "250ms", "2m".
fn parse_interval(raw: &str) -> Result<std::time::Duration, String> {
    use std::time::Duration;

    let trimmed = raw.trim();
    let (num_str, unit) = split_unit(trimmed);
    let num: u64 = num_str
        .parse()
        .map_err(|_| format!("cannot parse interval number in {raw:?}"))?;

    Ok(match unit {
        "ms" => Duration::from_millis(num),
        "s" | "" => Duration::from_secs(num),
        "m" => Duration::from_secs(num * 60),
        "h" => Duration::from_secs(num * 3600),
        other => {
            return Err(format!("unsupported interval unit {other:?} in {raw:?}"));
        }
    })
}

fn split_unit(s: &str) -> (&str, &str) {
    let idx = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    (&s[..idx], &s[idx..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_interval_variants() {
        assert_eq!(
            parse_interval("1s").unwrap(),
            std::time::Duration::from_secs(1)
        );
        assert_eq!(
            parse_interval("500ms").unwrap(),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            parse_interval("2m").unwrap(),
            std::time::Duration::from_secs(120)
        );
        assert_eq!(
            parse_interval("1h").unwrap(),
            std::time::Duration::from_secs(3600)
        );
        assert_eq!(
            parse_interval("42").unwrap(),
            std::time::Duration::from_secs(42)
        );
        assert!(parse_interval("5xyz").is_err());
    }
}
