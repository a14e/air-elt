use std::sync::Arc;

use tracing::info;

use crate::config::model::RootConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping::{self};
use crate::model::{ReadSpec, WriteSpec};
use crate::registry::Registry;
use crate::traits::{Sink, Source, Storage};
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
    pub query_timeout: std::time::Duration,
}

/// Stepped validator. Each step emits a typed error and logs the outcome.
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

        let source =
            registry
                .build_source(source_cfg)
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "source",
                    name: source_cfg.name.clone(),
                    source: Box::new(e),
                })?;
        let sink =
            registry
                .build_sink(sink_cfg)
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "sink",
                    name: sink_cfg.name.clone(),
                    source: Box::new(e),
                })?;
        let storage = registry.build_storage(storage_cfg).await.map_err(|e| {
            ValidationError::AccessFailed {
                component: "storage",
                name: storage_cfg.name.clone(),
                source: Box::new(e),
            }
        })?;

        let mappings = mapping::build(flow).map_err(|e| ValidationError::AccessFailed {
            component: "mapping",
            name: flow_name.clone(),
            source: Box::new(RuntimeError::Config(e)),
        })?;

        let read_spec = ReadSpec {
            columns: mappings.iter().map(|m| m.from.clone()).collect(),
            table: flow.from.clone(),
            cursor_fields: flow.cursor.fields.clone(),
            cursor_order: flow.cursor.order,
            limit: flow.batch_limit,
        };
        let write_spec = WriteSpec {
            columns: mappings.iter().map(|m| m.to.clone()).collect(),
            table: flow.to.clone(),
        };

        // Access checks — actual DB connections, ping SELECT 1 + insert-where-false etc.
        storage
            .validate_access()
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "storage",
                name: storage_cfg.name.clone(),
                source: Box::new(e),
            })?;
        source
            .validate_access(&read_spec)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "source",
                name: source_cfg.name.clone(),
                source: Box::new(e),
            })?;
        sink.validate_access(&write_spec)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "sink",
                name: sink_cfg.name.clone(),
                source: Box::new(e),
            })?;

        // Schemas + type matrix.
        let src_schema = source.describe_schema(&flow.from).await.map_err(|e| {
            ValidationError::AccessFailed {
                component: "source:schema",
                name: source_cfg.name.clone(),
                source: Box::new(e),
            }
        })?;
        let dst_schema =
            sink.describe_schema(&flow.to)
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "sink:schema",
                    name: sink_cfg.name.clone(),
                    source: Box::new(e),
                })?;

        checks::check_cursor(flow_name, &src_schema, &flow.cursor.fields)?;
        checks::check_mapping(&src_schema, &dst_schema, &mappings)?;

        let interval = flow.cursor.interval;

        let query_timeout = flow
            .query_timeout
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
            query_timeout,
        });
    }

    Ok(resolved)
}
