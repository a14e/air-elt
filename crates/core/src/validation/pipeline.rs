use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;
use futures::future::join_all;
use tracing::info;

use crate::config::model::ComponentConfig;

use crate::config::model::RootConfig;
use crate::config::validation::SamplingConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping::{self, ColumnMapping};
use crate::model::{AssembledFlow, ConversionPlan, FlowState, ReadSpec, WriteSpec};
use crate::registry::Registry;
use crate::traits::{Sink, Source, Storage};
use crate::types::{ConversionContext, DataType};
use crate::validation::{checks, sampling};

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
    // O(1) name → config indexes built once. Avoids the per-flow linear
    // scans that would make assemble O(F × (S + K + T)) on a config with
    // thousands of flows referencing the same handful of components.
    let source_index: AHashMap<&str, &ComponentConfig> =
        root.sources.iter().map(|c| (c.name.as_str(), c)).collect();
    let sink_index: AHashMap<&str, &ComponentConfig> =
        root.sinks.iter().map(|c| (c.name.as_str(), c)).collect();
    let storage_index: AHashMap<&str, &ComponentConfig> =
        root.storages.iter().map(|c| (c.name.as_str(), c)).collect();

    // Phase 1: walk all flows once, validate references, and collect
    // distinct component names per kind. BTreeSet keeps the subsequent
    // build order deterministic for stable logs — set size is the
    // number of declared components, not the flow count, so the
    // ordering cost is negligible.
    let mut source_names: BTreeSet<&str> = BTreeSet::new();
    let mut sink_names: BTreeSet<&str> = BTreeSet::new();
    let mut storage_names: BTreeSet<&str> = BTreeSet::new();
    for flow in root.flow.values() {
        let source_name = flow.source.name();
        if !source_index.contains_key(source_name) {
            return Err(ValidationError::UnknownSource(source_name.to_string()));
        }
        if !sink_index.contains_key(flow.sink.as_str()) {
            return Err(ValidationError::UnknownSink(flow.sink.clone()));
        }
        if !storage_index.contains_key(flow.storage.as_str()) {
            return Err(ValidationError::UnknownStorage(flow.storage.clone()));
        }
        source_names.insert(source_name);
        sink_names.insert(flow.sink.as_str());
        storage_names.insert(flow.storage.as_str());
    }

    // Phase 2: build each referenced component exactly once via O(1)
    // index lookup. Unreferenced components are skipped.
    let mut sources: AHashMap<&str, Arc<dyn Source>> = AHashMap::new();
    for &name in &source_names {
        let cfg = source_index[name];
        let built: Arc<dyn Source> = Arc::from(registry.build_source(cfg).await.map_err(|e| {
            ValidationError::AccessFailed {
                component: "source",
                name: cfg.name.clone(),
                source: Box::new(e),
            }
        })?);
        sources.insert(name, built);
    }
    let mut sinks: AHashMap<&str, Arc<dyn Sink>> = AHashMap::new();
    for &name in &sink_names {
        let cfg = sink_index[name];
        let built: Arc<dyn Sink> = Arc::from(registry.build_sink(cfg).await.map_err(|e| {
            ValidationError::AccessFailed {
                component: "sink",
                name: cfg.name.clone(),
                source: Box::new(e),
            }
        })?);
        sinks.insert(name, built);
    }
    let mut storages: AHashMap<&str, Arc<dyn Storage>> = AHashMap::new();
    for &name in &storage_names {
        let cfg = storage_index[name];
        let built: Arc<dyn Storage> =
            Arc::from(registry.build_storage(cfg).await.map_err(|e| {
                ValidationError::AccessFailed {
                    component: "storage",
                    name: cfg.name.clone(),
                    source: Box::new(e),
                }
            })?);
        storages.insert(name, built);
    }

    // Phase 3: assemble each flow by attaching shared Arcs via O(1)
    // map lookups.
    let mut flows = Vec::with_capacity(root.flow.len());
    for (flow_name, flow) in &root.flow {
        info!(flow = %flow_name, "assembling flow");

        let source_name = flow.source.name();
        let source_cfg = source_index[source_name];
        let source = sources[source_name].clone();
        let sink = sinks[flow.sink.as_str()].clone();
        let storage = storages[flow.storage.as_str()].clone();

        // Per-source-kind cursor.fields shape. Pull-based connectors
        // (postgres/mysql/mongodb) require non-empty cursor.fields so
        // each batch has a deterministic high-water mark; CDC
        // connectors (mongo-cdc) refuse cursor.fields because
        // pagination is driven by the resume token they manage
        // themselves. Doing this here instead of in the loader because
        // only assemble has access to the source's `kind`.
        let kind = source_cfg.kind.as_str();
        let is_cdc = matches!(kind, "mongo-cdc");
        if is_cdc {
            if !flow.cursor.fields.is_empty() {
                return Err(ValidationError::AccessFailed {
                    component: "flow",
                    name: flow_name.clone(),
                    source: Box::new(RuntimeError::Other(format!(
                        "flow {flow_name:?}: source kind {kind:?} does not accept user cursors \
                         — remove `cursor.fields` (the resume token replaces it)"
                    ))),
                });
            }
            if flow.conflict.is_none() {
                return Err(ValidationError::AccessFailed {
                    component: "flow",
                    name: flow_name.clone(),
                    source: Box::new(RuntimeError::Other(format!(
                        "flow {flow_name:?}: source kind {kind:?} requires a `[flow.{flow_name}.conflict]` \
                         block — cdc emits Upsert/Delete which need a key"
                    ))),
                });
            }
        } else if flow.cursor.fields.is_empty() {
            return Err(ValidationError::AccessFailed {
                component: "flow",
                name: flow_name.clone(),
                source: Box::new(RuntimeError::Other(format!(
                    "flow {flow_name:?}: source kind {kind:?} requires non-empty `cursor.fields`"
                ))),
            });
        }

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
            source_options: flow.source.options(),
        };
        let write_spec = WriteSpec {
            columns: mappings.iter().map(|m| m.to.clone()).collect(),
            table: flow.to.clone(),
            conflict: flow.conflict.clone(),
        };

        let interval = flow.cursor.interval;
        let query_timeout = flow.query_timeout.unwrap_or(DEFAULT_QUERY_TIMEOUT);

        let backend_default = registry.sampling_default(&source_cfg.kind);
        let sampling = flow.validation.sampling.resolve(backend_default);

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
            sampling,
            access_check: flow.validation.access,
            fields_check: flow.validation.fields,
            inserts_check: flow.validation.inserts,
            cursor_persistence: if is_cdc {
                crate::model::CursorPersistence::ResumeToken
            } else {
                crate::model::CursorPersistence::ColumnCursor
            },
        });
    }

    Ok(flows)
}

/// I/O validation: access checks, schema introspection, type compatibility,
/// and (when configured) sampling-validation.
///
/// Flows are grouped by source and validated in parallel — one async
/// worker per source, sequential within a source. The latter matters
/// because flows that share a source pool would otherwise contend on
/// the same connections. We log the worker count to stdout via the
/// `tracing` info channel so operators can see how the work was
/// scheduled.
///
/// Output ordering is deterministic: results are merged back into the
/// flows' original config-order, so a reproducible failure produces
/// reproducible CLI output regardless of which worker finished first.
pub async fn validate(assembled: Vec<AssembledFlow>) -> Result<Vec<FlowState>, ValidationError> {
    if assembled.is_empty() {
        return Ok(Vec::new());
    }

    // Track each flow's original index to preserve config order on
    // output. Group by source name with first-seen ordering.
    let total = assembled.len();
    let mut group_order: Vec<String> = Vec::new();
    let mut groups: AHashMap<String, Vec<(usize, AssembledFlow)>> = AHashMap::new();
    for (idx, flow) in assembled.into_iter().enumerate() {
        let source_name = flow.source.name().to_string();
        if !groups.contains_key(&source_name) {
            group_order.push(source_name.clone());
        }
        groups.entry(source_name).or_default().push((idx, flow));
    }

    let workers = group_order.len();
    info!(workers, "running validation in {workers} workers");

    let mut futures = Vec::with_capacity(workers);
    for source_name in &group_order {
        let flows = groups.remove(source_name).unwrap_or_default();
        futures.push(validate_source_group(flows));
    }

    let results = join_all(futures).await;
    let mut indexed: Vec<(usize, FlowState)> = Vec::with_capacity(total);
    for group_result in results {
        match group_result {
            Ok(states) => indexed.extend(states),
            Err(e) => return Err(e),
        }
    }
    indexed.sort_by_key(|(idx, _)| *idx);
    Ok(indexed.into_iter().map(|(_, s)| s).collect())
}

async fn validate_source_group(
    flows: Vec<(usize, AssembledFlow)>,
) -> Result<Vec<(usize, FlowState)>, ValidationError> {
    let mut out = Vec::with_capacity(flows.len());
    for (idx, flow) in flows {
        let state = validate_one(flow).await?;
        out.push((idx, state));
    }
    Ok(out)
}

/// Build identity passthrough plans (`source = sink = Json`,
/// `ConversionContext::passthrough`) used when `validation.fields = false`.
/// Each plan is `is_identity()` so the runner skips per-cell `convert`
/// entirely. `truncate` is a documented no-op on passthrough plans
/// (passthrough does not narrow); `default` is rejected because parsing
/// the literal needs the real sink type, which is unknown without
/// schema introspection.
fn passthrough_plans(
    flow_name: &str,
    mappings: &[ColumnMapping],
) -> Result<Vec<ConversionPlan>, ValidationError> {
    mappings
        .iter()
        .map(|m| {
            if m.default_literal.is_some() {
                return Err(ValidationError::DefaultRequiresFields {
                    flow: flow_name.to_string(),
                    column: m.from.clone(),
                });
            }
            // Why: passthrough doesn't narrow, so `truncate` is a no-op
            // when fields_check is off. Forcing `truncate = false` keeps
            // the plan `is_identity()`, which lets the runner short-
            // circuit per-cell convert.
            Ok(ConversionPlan {
                source: DataType::Json,
                sink: DataType::Json,
                ctx: ConversionContext::passthrough(),
            })
        })
        .collect()
}

async fn validate_one(flow: AssembledFlow) -> Result<FlowState, ValidationError> {
    info!(flow = %flow.name, "validating flow");

    if flow.access_check {
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
    } else {
        info!(flow = %flow.name, "validation.access disabled — skipping source/storage probes");
    }
    if flow.inserts_check {
        flow.sink
            .validate_access(&flow.write_spec)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "sink",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;
        // Why: insert-only `validate_access` does not exercise the DELETE
        // SQL/operation a CDC flow will eventually issue. Pre-flight it
        // here so a missing DELETE privilege surfaces at validate-time
        // instead of on the first delete batch. Gated on `conflict.is_some()`
        // because the sink's delete path is keyed on conflict.key — without
        // it the runner would refuse the Delete row anyway.
        if flow.source.emits_deletes() && flow.write_spec.conflict.is_some() {
            flow.sink
                .validate_delete_access(&flow.write_spec)
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "sink:delete",
                    name: flow.name.clone(),
                    source: Box::new(e),
                })?;
        }
    } else {
        info!(flow = %flow.name, "validation.inserts disabled — skipping sink write probe");
    }

    let conversions: Vec<ConversionPlan> = if flow.fields_check {
        let src_schema = flow
            .source
            .describe_schema(&flow.read_spec.table)
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "source:schema",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;
        let dst_schema = if flow.sink.schemaless() {
            // Schemaless sinks (MongoDB) inherit the source's declared
            // types so the matrix check is a no-op identity. Nullability
            // is set to `true` (the sink can always accept a missing
            // value).
            crate::model::Schema::new(
                flow.mappings
                    .iter()
                    .filter_map(|m| {
                        src_schema
                            .find(&m.from)
                            .map(|src_field| crate::model::Field {
                                name: m.to.clone(),
                                data_type: src_field.data_type.clone(),
                                nullable: true,
                            })
                    })
                    .collect(),
            )
        } else {
            flow.sink
                .describe_schema(&flow.write_spec.table)
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "sink:schema",
                    name: flow.name.clone(),
                    source: Box::new(e),
                })?
        };

        checks::check_cursor(&flow.name, &src_schema, &flow.read_spec.cursor_fields)?;
        checks::check_mapping(&src_schema, &dst_schema, &flow.mappings)?;

        let flow_name = flow.name.clone();
        flow.mappings
            .iter()
            .map(|m| {
                let src_field =
                    src_schema
                        .find(&m.from)
                        .ok_or_else(|| ValidationError::MissingField {
                            side: "source",
                            field: m.from.clone(),
                        })?;
                let sink_dt = dst_schema
                    .find(&m.to)
                    .map(|f| f.data_type.clone())
                    .ok_or_else(|| ValidationError::MissingField {
                        side: "sink",
                        field: m.to.clone(),
                    })?;
                // `default` on a NOT NULL source column is meaningless —
                // reject at validate time before the runner ever sees it.
                if m.default_literal.is_some() && !src_field.nullable {
                    return Err(ValidationError::DefaultOnNotNullSource {
                        flow: flow_name.clone(),
                        column: m.from.clone(),
                    });
                }
                let parsed_default = match &m.default_literal {
                    Some(lit) => Some(crate::types::default_value::parse(lit, &sink_dt).map_err(
                        |e| ValidationError::DefaultParse {
                            flow: flow_name.clone(),
                            column: m.from.clone(),
                            source: e,
                        },
                    )?),
                    None => None,
                };
                let mut ctx = ConversionContext::passthrough();
                ctx.truncate = m.truncate;
                ctx.default = parsed_default;
                Ok(ConversionPlan {
                    source: src_field.data_type.clone(),
                    sink: sink_dt,
                    ctx,
                })
            })
            .collect::<Result<_, ValidationError>>()?
    } else {
        info!(
            flow = %flow.name,
            "validation.fields disabled — skipping schema introspection; conversions are passthrough"
        );
        passthrough_plans(&flow.name, &flow.mappings)?
    };

    if let SamplingConfig::Enabled { size } = flow.sampling {
        sampling::run(&flow, &conversions, size).await?;
    }

    info!(flow = %flow.name, "flow validated");
    Ok(FlowState::new(flow, conversions))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::model::CursorOrder;
    use crate::config::validation::SamplingConfig;
    use crate::error::ValidationError;
    use crate::mapping::ColumnMapping;
    use crate::model::{AssembledFlow, ReadSpec, WriteSpec};
    use crate::traits::{MockSink, MockSource, MockStorage};
    use crate::types::DataType;

    use super::*;

    fn flow_with(
        source: MockSource,
        sink: MockSink,
        storage: MockStorage,
        mappings: Vec<ColumnMapping>,
        fields_check: bool,
    ) -> AssembledFlow {
        AssembledFlow {
            name: "test".into(),
            source: Arc::new(source),
            sink: Arc::new(sink),
            storage: Arc::new(storage),
            mappings,
            read_spec: ReadSpec {
                columns: vec!["a".into()],
                table: "t".into(),
                cursor_fields: vec!["a".into()],
                cursor_order: CursorOrder::Asc,
                limit: 1,
                source_options: toml::Table::new(),
            },
            write_spec: WriteSpec {
                columns: vec!["a".into()],
                table: "t".into(),
                conflict: None,
            },
            interval: Duration::from_millis(10),
            query_timeout: Duration::from_secs(5),
            sampling: SamplingConfig::Disabled,
            access_check: false,
            fields_check,
            inserts_check: false,
            cursor_persistence: crate::model::CursorPersistence::ColumnCursor,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fields_check_false_skips_describe_schema() {
        // Operator scenario: a brand-new Mongo collection with zero
        // documents. Sample-based inference would fail on it, so the
        // operator opts into `fields = false`. The contract proven
        // here — describe_schema is *not* invoked — is exactly what
        // makes that scenario safe; an end-to-end test against an
        // empty collection would only re-verify the same mock count.
        let mut source = MockSource::new();
        source.expect_name().return_const("src".to_string());
        // Critical: introspection must NOT run.
        source.expect_describe_schema().times(0);
        let mut sink = MockSink::new();
        sink.expect_describe_schema().times(0);
        // With fields_check=false the sink's schemaless() is never
        // queried — assert that explicitly to catch future regressions.
        sink.expect_schemaless().times(0);
        let storage = MockStorage::new();

        let mappings = vec![ColumnMapping {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: None,
        }];
        let flow = flow_with(source, sink, storage, mappings, false);

        let state = validate_one(flow).await.unwrap();
        assert_eq!(state.conversions.len(), 1);
        let plan = &state.conversions[0];
        assert_eq!(plan.source, DataType::Json);
        assert_eq!(plan.sink, DataType::Json);
        assert!(plan.is_identity());
    }

    fn flow_with_inserts_and_conflict(
        source: MockSource,
        sink: MockSink,
        storage: MockStorage,
        conflict: Option<crate::config::conflict::ConflictConfig>,
    ) -> AssembledFlow {
        let mappings = vec![ColumnMapping {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: None,
        }];
        let mut flow = flow_with(source, sink, storage, mappings, false);
        flow.inserts_check = true;
        flow.write_spec.conflict = conflict;
        flow
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_invoked_when_source_emits_deletes_and_conflict_set() {
        let mut source = MockSource::new();
        source.expect_name().return_const("src".to_string());
        source.expect_emits_deletes().return_const(true);
        let mut sink = MockSink::new();
        sink.expect_validate_access().times(1).returning(|_| Ok(()));
        sink.expect_validate_delete_access()
            .times(1)
            .returning(|_| Ok(()));
        let storage = MockStorage::new();
        let flow = flow_with_inserts_and_conflict(
            source,
            sink,
            storage,
            Some(crate::config::conflict::ConflictConfig {
                key: vec!["a".into()],
                strategy: crate::config::conflict::ConflictStrategy::Overwrite,
            }),
        );
        validate_one(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_skipped_when_source_does_not_emit_deletes() {
        let mut source = MockSource::new();
        source.expect_name().return_const("src".to_string());
        source.expect_emits_deletes().return_const(false);
        let mut sink = MockSink::new();
        sink.expect_validate_access().times(1).returning(|_| Ok(()));
        sink.expect_validate_delete_access().times(0);
        let storage = MockStorage::new();
        let flow = flow_with_inserts_and_conflict(
            source,
            sink,
            storage,
            Some(crate::config::conflict::ConflictConfig {
                key: vec!["a".into()],
                strategy: crate::config::conflict::ConflictStrategy::Overwrite,
            }),
        );
        validate_one(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_skipped_without_conflict_block() {
        let mut source = MockSource::new();
        source.expect_name().return_const("src".to_string());
        // emits_deletes may or may not be queried — sink-side gating
        // is the conflict.is_some() check. We only assert
        // validate_delete_access is not called.
        source.expect_emits_deletes().return_const(true);
        let mut sink = MockSink::new();
        sink.expect_validate_access().times(1).returning(|_| Ok(()));
        sink.expect_validate_delete_access().times(0);
        let storage = MockStorage::new();
        let flow = flow_with_inserts_and_conflict(source, sink, storage, None);
        validate_one(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fields_check_false_with_default_rejected() {
        let mut source = MockSource::new();
        source.expect_name().return_const("src".to_string());
        source.expect_describe_schema().times(0);
        let mut sink = MockSink::new();
        sink.expect_describe_schema().times(0);
        sink.expect_schemaless().return_const(false);
        let storage = MockStorage::new();

        let mappings = vec![ColumnMapping {
            from: "a".into(),
            to: "a".into(),
            truncate: false,
            default_literal: Some(toml::Value::String("x".into())),
        }];
        let flow = flow_with(source, sink, storage, mappings, false);

        let res = validate_one(flow).await;
        let err = match res {
            Ok(_) => panic!("expected DefaultRequiresFields, got Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ValidationError::DefaultRequiresFields { .. }),
            "expected DefaultRequiresFields, got {err:?}"
        );
    }
}
