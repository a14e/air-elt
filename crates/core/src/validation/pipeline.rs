use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;
use tracing::info;

use crate::config::model::RootConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping;
use crate::model::{AssembledFlow, FlowState, ReadSpec, WriteSpec};
use crate::registry::Registry;
use crate::traits::{Sink, Source, Storage};
use crate::validation::checks;

const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Assemble flows from config: look up components, build via registry,
/// construct ReadSpec/WriteSpec. No I/O validation — just wiring.
///
/// Sources / sinks / storages are shared by name across flows: building each
/// instance only once means a single pool per declared component, regardless
/// of how many flows reference it.
pub async fn assemble(
    root: &RootConfig,
    registry: &Registry,
) -> Result<Vec<AssembledFlow>, ValidationError> {
    let mut flows = Vec::with_capacity(root.flow.len());
    let mut sources: AHashMap<String, Arc<dyn Source>> = AHashMap::new();
    let mut sinks: AHashMap<String, Arc<dyn Sink>> = AHashMap::new();
    let mut storages: AHashMap<String, Arc<dyn Storage>> = AHashMap::new();

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

        let source = if let Some(existing) = sources.get(&source_cfg.name) {
            existing.clone()
        } else {
            let built: Arc<dyn Source> =
                Arc::from(registry.build_source(source_cfg).await.map_err(|e| {
                    ValidationError::AccessFailed {
                        component: "source",
                        name: source_cfg.name.clone(),
                        source: Box::new(e),
                    }
                })?);
            sources.insert(source_cfg.name.clone(), built.clone());
            built
        };
        let sink = if let Some(existing) = sinks.get(&sink_cfg.name) {
            existing.clone()
        } else {
            let built: Arc<dyn Sink> =
                Arc::from(registry.build_sink(sink_cfg).await.map_err(|e| {
                    ValidationError::AccessFailed {
                        component: "sink",
                        name: sink_cfg.name.clone(),
                        source: Box::new(e),
                    }
                })?);
            sinks.insert(sink_cfg.name.clone(), built.clone());
            built
        };
        let storage = if let Some(existing) = storages.get(&storage_cfg.name) {
            existing.clone()
        } else {
            let built: Arc<dyn Storage> =
                Arc::from(registry.build_storage(storage_cfg).await.map_err(|e| {
                    ValidationError::AccessFailed {
                        component: "storage",
                        name: storage_cfg.name.clone(),
                        source: Box::new(e),
                    }
                })?);
            storages.insert(storage_cfg.name.clone(), built.clone());
            built
        };

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

        flows.push(AssembledFlow {
            name: flow_name.clone(),
            source,
            sink,
            storage,
            mappings,
            read_spec,
            write_spec,
            interval,
            query_timeout,
        });
    }

    Ok(flows)
}

/// I/O validation: access checks, schema introspection, type compatibility.
/// Consumes assembled flows and returns validated `FlowState`s with their
/// per-column conversions populated. The two-stage typing (`AssembledFlow`
/// → `FlowState`) makes "skipped validation" unrepresentable for the
/// runner.
pub async fn validate(assembled: Vec<AssembledFlow>) -> Result<Vec<FlowState>, ValidationError> {
    let mut out = Vec::with_capacity(assembled.len());
    for flow in assembled {
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
        checks::check_mapping(&src_schema, &dst_schema, &flow.mappings)?;

        let conversions: Vec<(crate::types::DataType, crate::types::DataType)> = flow
            .mappings
            .iter()
            .map(|m| {
                let src_dt = src_schema
                    .find(&m.from)
                    .map(|f| f.data_type)
                    .ok_or_else(|| ValidationError::MissingField {
                        side: "source",
                        field: m.from.clone(),
                    })?;
                let sink_dt = dst_schema.find(&m.to).map(|f| f.data_type).ok_or_else(|| {
                    ValidationError::MissingField {
                        side: "sink",
                        field: m.to.clone(),
                    }
                })?;
                Ok((src_dt, sink_dt))
            })
            .collect::<Result<_, ValidationError>>()?;

        info!(flow = %flow.name, "flow validated");
        out.push(FlowState::new(flow, conversions));
    }

    Ok(out)
}
