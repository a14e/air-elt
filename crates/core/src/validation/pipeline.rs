use std::time::Duration;

use tracing::info;

use crate::config::model::RootConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping;
use crate::model::{FlowState, ReadSpec, WriteSpec};
use crate::registry::Registry;
use crate::validation::checks;

const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Assemble flows from config: look up components, build via registry,
/// construct ReadSpec/WriteSpec. No I/O validation — just wiring.
pub async fn assemble(
    root: &RootConfig,
    registry: &Registry,
) -> Result<Vec<FlowState>, ValidationError> {
    let mut flows = Vec::with_capacity(root.flow.len());

    for (flow_name, flow) in &root.flow {
        info!(flow = %flow_name, "assembling flow");

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

        let interval = flow.cursor.interval;
        let query_timeout = flow.query_timeout.unwrap_or(DEFAULT_QUERY_TIMEOUT);

        flows.push(FlowState {
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

    Ok(flows)
}

/// I/O validation: access checks, schema introspection, type compatibility.
pub async fn validate(flows: &[FlowState]) -> Result<(), ValidationError> {
    for flow in flows {
        info!(flow = %flow.name, "validating flow");

        flow.storage
            .validate_access()
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "storage",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;
        flow.source
            .validate_access(&flow.read_spec)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "source",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;
        flow.sink
            .validate_access(&flow.write_spec)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "sink",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;

        let src_schema = flow
            .source
            .describe_schema(&flow.read_spec.table)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "source:schema",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;
        let dst_schema = flow
            .sink
            .describe_schema(&flow.write_spec.table)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "sink:schema",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;

        checks::check_cursor(&flow.name, &src_schema, &flow.read_spec.cursor_fields)?;

        // Invariant: read_spec.columns and write_spec.columns are positionally
        // paired by assemble() — both derived from the same ordered mappings vec.
        let mappings: Vec<crate::mapping::ColumnMapping> = flow
            .read_spec
            .columns
            .iter()
            .zip(flow.write_spec.columns.iter())
            .map(|(from, to)| crate::mapping::ColumnMapping {
                from: from.clone(),
                to: to.clone(),
            })
            .collect();
        checks::check_mapping(&src_schema, &dst_schema, &mappings)?;

        info!(flow = %flow.name, "flow validated");
    }

    Ok(())
}
