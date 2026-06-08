//! Validation pipeline — `assemble` (no I/O) + `validate` (probes,
//! schema introspection, matrix, sampling).
//!
//! ## Schemaless sources
//!
//! When `Source::schemaless() == true` (Mongo and its CDC variant),
//! the sampled schema is treated as a validation-time **hypothesis**,
//! not a runtime contract. Consequences:
//!
//! * The per-flow `[flow.<name>.validation] fields` toggle, if left
//!   unset by the operator, resolves to `false` for schemaless sources
//!   — the matrix narrowing check and nullability check are skipped.
//!   Access probes, sink-side checks, and cursor presence still run.
//! * The Transform compiler emits dynamic-source `TransformOp::Convert`
//!   ops (with `plan.source = None`); the runtime resolves the source
//!   `DataType` per cell from the actual `Value` variant.
//! * Explicit `fields = true` is still honoured — operators who want
//!   the matrix check against the sampled schema opt in there.
//!
//! For typed sources (pg, mysql, ...) `information_schema` is the
//! contract and the static plan path is left as-is.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use ahash::AHashMap;
use futures::StreamExt;
use tracing::info;

use crate::config::model::ComponentConfig;

use crate::config::model::RootConfig;
use crate::config::validation::SamplingConfig;
use crate::error::{RuntimeError, ValidationError};
use crate::mapping::{self, ColumnMapping, DirectMapping};
use crate::model::{
    AssembledFlow, ColumnConversionPlan, ConfigReadSpec, ConfigWriteSpec, DerivedPlans, FlowState,
    ReadSpec, Schema, WriteSpec,
};
use crate::registry::Registry;
use crate::traits::{Sink, Source, Storage};
use crate::types::{ConversionContext, DataType};
use crate::validation::checks;
use crate::validation::compatibility::CompatibilityValidator;

use crate::util::retry_transient;

const DEFAULT_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Assemble flows from config: look up components, build via registry,
/// construct ReadSpec/WriteSpec. No I/O validation — just wiring.
///
/// Sources / sinks / storages are shared by name across flows: building each
/// instance only once means a single pool per declared component, regardless
/// of how many flows reference it.
///
/// `config_dir` is used to build the expression evaluation context for
/// resolving `default = "env('KEY', 'fallback')"` style expressions.
/// Pass `None` when expressions should not be evaluated (e.g. in tests
/// that build flows synthetically).
pub async fn assemble(
    root: &RootConfig,
    registry: &Registry,
    monitoring: &mut air_elt_monitoring::MonitoringManager,
    config_dir: Option<&std::path::Path>,
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

    let expr_context: Arc<air_elt_expr_runtime::ExpressionContext> =
        Arc::new(air_elt_expr_runtime::ExpressionContext::create(
            registry.expr_functions().clone(),
            config_dir.unwrap_or(std::path::Path::new(".")),
        ));

    // Phase 2: build each referenced component exactly once via O(1)
    // index lookup. Unreferenced components are skipped. Alongside the
    // build we register each component in the shared
    // `ConcurrencyManager` with permits = `max_connections()`. The
    // runner contract is "no permit held across two backend calls", so
    // there is no canonical lock order to maintain — deadlock between
    // flows is structurally impossible. See the project-conventions
    // skill ("Concurrency: per-component semaphores").
    let mut concurrency = crate::util::ConcurrencyManager::new();
    let mut sources: AHashMap<&str, Arc<dyn Source>> = AHashMap::new();
    for &name in &source_names {
        let cfg = source_index[name];
        let built: Arc<dyn Source> =
            Arc::from(registry.build_source(cfg, monitoring).await.map_err(|e| {
                ValidationError::AccessFailed {
                    component: "source",
                    name: cfg.name.clone(),
                    source: Box::new(e),
                }
            })?);
        let max = built.max_connections();
        concurrency.register_source(name, max);
        monitoring.set_lock_max(air_elt_monitoring::ComponentKind::Source, name, max);
        sources.insert(name, built);
    }
    let mut sinks: AHashMap<&str, Arc<dyn Sink>> = AHashMap::new();
    for &name in &sink_names {
        let cfg = sink_index[name];
        let built: Arc<dyn Sink> =
            Arc::from(registry.build_sink(cfg, monitoring).await.map_err(|e| {
                ValidationError::AccessFailed {
                    component: "sink",
                    name: cfg.name.clone(),
                    source: Box::new(e),
                }
            })?);
        let max = built.max_connections();
        concurrency.register_sink(name, max);
        monitoring.set_lock_max(air_elt_monitoring::ComponentKind::Sink, name, max);
        sinks.insert(name, built);
    }
    let mut storages: AHashMap<&str, Arc<dyn Storage>> = AHashMap::new();
    for &name in &storage_names {
        let cfg = storage_index[name];
        let built: Arc<dyn Storage> =
            Arc::from(registry.build_storage(cfg, monitoring).await.map_err(|e| {
                ValidationError::AccessFailed {
                    component: "storage",
                    name: cfg.name.clone(),
                    source: Box::new(e),
                }
            })?);
        let max = built.max_connections();
        concurrency.register_storage(name, max);
        monitoring.set_lock_max(air_elt_monitoring::ComponentKind::Storage, name, max);
        storages.insert(name, built);
    }
    // assemble is done populating the manager — log per-component
    // budgets once so an operator can verify the configured
    // `max-connections` lined up with the live semaphore caps. After
    // this the manager is read-only; we only use it to issue
    // per-flow `FlowLockHandle`s below.
    crate::util::log_concurrency_budgets(&concurrency);

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
        let sink_cfg = sink_index[flow.sink.as_str()];
        let storage_cfg = storage_index[flow.storage.as_str()];

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
            // Append-only sinks (`supports_deletes() == false`) make
            // the `[flow.x.conflict]` block optional for CDC sources:
            // deletes get filtered pre-write inside the runner, so the
            // sink only ever sees inserts. Without a conflict block
            // we accept plain INSERT per CDC event — append-only ingest.
            if flow.conflict.is_none() && sink.supports_deletes() {
                return Err(ValidationError::AccessFailed {
                    component: "flow",
                    name: flow_name.clone(),
                    source: Box::new(RuntimeError::Other(format!(
                        "flow {flow_name:?}: source kind {kind:?} requires a `[flow.{flow_name}.conflict]` \
                         block — cdc emits Upsert/Delete which need a key (unless the sink declares \
                         `supports_deletes = false`, in which case append-only ingest is allowed)"
                    ))),
                });
            }
        } else if flow.cursor.fields.is_empty() {
            // Raw-passthrough flows (`mapping = ["*"]` with both sides
            // schemaless) deliberately reject `cursor.fields` —
            // there are no direct columns to cursor on. Detect
            // that shape pre-`mapping::build` so a legitimate raw flow
            // doesn't trip this kind-level guard. The actual rejection
            // of any *non-empty* `cursor.fields` for raw flows happens
            // in `validate_flow` after expansion (see
            // `CursorRequiresExplicitFields`); here we only need to
            // permit the empty-fields case.
            let raw_passthrough_eligible = source.schemaless()
                && sink.schemaless()
                && flow.mapping.iter().any(|(key, rhs)| {
                    key == "*"
                        && matches!(
                            rhs,
                            crate::config::model::MappingRhs::Short(s) if s == "*"
                        )
                });
            if !raw_passthrough_eligible {
                return Err(ValidationError::AccessFailed {
                    component: "flow",
                    name: flow_name.clone(),
                    source: Box::new(RuntimeError::Other(format!(
                        "flow {flow_name:?}: source kind {kind:?} requires non-empty `cursor.fields`"
                    ))),
                });
            }
        }

        let rules = mapping::build(flow).map_err(|e| ValidationError::AccessFailed {
            component: "mapping",
            name: flow_name.clone(),
            source: Box::new(RuntimeError::Config(e)),
        })?;

        // Schema-independent halves of the read/write specs. The
        // post-expansion `ReadSpec` / `WriteSpec` (with `columns` and
        // `needs_body`) is built later in `validate_flow` once schemas
        // are available and materialised onto `DerivedPlans`.
        let config_read_spec = ConfigReadSpec {
            table: flow.from.clone(),
            cursor_fields: flow.cursor.fields.clone(),
            cursor_order: flow.cursor.order,
            limit: flow.batch_limit,
            source_options: flow.source.options(),
        };
        let config_write_spec = ConfigWriteSpec {
            table: flow.to.clone(),
            conflict: flow.conflict.clone(),
        };

        let interval = flow.cursor.interval;
        // Resolve `cursor.jitter` once at assemble — the loader has
        // already validated its upper bound against `interval`; the
        // default (`min(interval, 5min)`) is applied here when the
        // operator omitted the field.
        let jitter = flow.cursor.effective_jitter();
        let query_timeout = flow.query_timeout.unwrap_or(DEFAULT_QUERY_TIMEOUT);

        let backend_default = registry.sampling_default(&source_cfg.kind);
        let sampling = flow.validation.sampling.resolve(backend_default);

        flows.push(AssembledFlow {
            name: flow_name.clone(),
            source,
            sink,
            storage,
            rules,
            config_read_spec,
            config_write_spec,
            interval,
            jitter,
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
            // Per-flow lock handle: pre-resolves the three (kind,
            // name) keys against the shared manager and caches the
            // resolved `Arc<Semaphore>` triple. Each `acquire_*` is a
            // single semaphore acquire — no per-tick hashmap lookup.
            // No call site ever holds two permits at once, so there is
            // no canonical lock order to maintain.
            lock_handle: concurrency.handle(
                source_name,
                flow.sink.as_str(),
                flow.storage.as_str(),
                monitoring,
            ),
            recorder: monitoring.flow_recorder(air_elt_monitoring::FlowLabels {
                flow: flow_name.clone(),
                source_name: source_name.to_string(),
                source_kind: source_cfg.kind.clone(),
                sink_name: flow.sink.clone(),
                sink_kind: sink_cfg.kind.clone(),
                storage_name: flow.storage.clone(),
                storage_kind: storage_cfg.kind.clone(),
            }),
            expr_context: expr_context.clone(),
        });
    }

    Ok(flows)
}

/// I/O validation: access checks, schema introspection, type compatibility,
/// and (when configured) sampling-validation.
///
/// **Concurrency model.** Every assembled flow is fed through one
/// `futures::stream::iter(...).for_each_concurrent(None, …)` — no
/// per-source grouping. Backend contention is bounded purely by the
/// per-component `tokio::sync::Semaphore`s built in
/// [`assemble`], sized to each backend's `max-connections`. Each flow
/// acquires permits on its source / sink / storage **before** any
/// I/O, in canonical `(component_type, component_name)` order so
/// flows with overlapping permit sets cannot deadlock. See the
/// `project-conventions` skill ("Validation concurrency: semaphores +
/// canonical-order acquire").
///
/// Output ordering is deterministic: results are merged back into the
/// flows' original config-order, so a reproducible failure produces
/// reproducible CLI output regardless of which task finished first.
pub async fn validate(assembled: Vec<AssembledFlow>) -> Result<Vec<FlowState>, ValidationError> {
    if assembled.is_empty() {
        return Ok(Vec::new());
    }

    let total = assembled.len();

    info!(
        flows = total,
        "running validation for {total} flows (semaphores cap concurrency per component)"
    );

    let results: Arc<tokio::sync::Mutex<Vec<(usize, FlowState)>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::with_capacity(total)));
    // Collect ALL errors with their config-order index, sort, return
    // the lowest. Using "first finisher" (the previous design) made
    // the surfaced error nondeterministic — two equally invalid flows
    // would surface whichever completed first, differing between runs.
    let errors: Arc<tokio::sync::Mutex<Vec<(usize, ValidationError)>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let any_error: Arc<std::sync::atomic::AtomicBool> =
        Arc::new(std::sync::atomic::AtomicBool::new(false));

    {
        let results = results.clone();
        let errors = errors.clone();
        let any_error = any_error.clone();
        let indexed = assembled.into_iter().enumerate();
        futures::stream::iter(indexed)
            .for_each_concurrent(None, |(idx, flow)| {
                let results = results.clone();
                let errors = errors.clone();
                let any_error = any_error.clone();
                async move {
                    // Short-circuit: once any flow has produced a hard
                    // error we stop spending budget on probes that
                    // haven't started yet. In-flight probes still
                    // finish — that's what makes the error set
                    // deterministic regardless of scheduler timing.
                    if any_error.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    match validate_flow(flow).await {
                        Ok(state) => {
                            results.lock().await.push((idx, state));
                        }
                        Err(e) => {
                            any_error.store(true, std::sync::atomic::Ordering::Relaxed);
                            errors.lock().await.push((idx, e));
                        }
                    }
                }
            })
            .await;
    }

    let mut collected_errors = Arc::try_unwrap(errors)
        .map_err(|_| ())
        .expect("validate: errors Arc must be unique after for_each_concurrent")
        .into_inner();
    if !collected_errors.is_empty() {
        // Sort by config-order index and return the lowest. Stable
        // across reruns: the same two invalid flows always surface
        // the same first error.
        collected_errors.sort_by_key(|(idx, _)| *idx);
        return Err(collected_errors.into_iter().next().expect("non-empty").1);
    }
    let mut indexed = Arc::try_unwrap(results)
        .map_err(|_| ())
        .expect("validate: results Arc must be unique after for_each_concurrent")
        .into_inner();
    indexed.sort_by_key(|(idx, _)| *idx);
    Ok(indexed.into_iter().map(|(_, s)| s).collect())
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
    direct: &[DirectMapping],
) -> Result<Vec<ColumnConversionPlan>, ValidationError> {
    direct
        .iter()
        .map(|m| {
            if m.compute.is_some() {
                return Err(ValidationError::ComputeRequiresFields {
                    flow: flow_name.to_string(),
                    column: m.to.clone(),
                });
            }
            if m.default_literal.is_some() {
                return Err(ValidationError::DefaultRequiresFields {
                    flow: flow_name.to_string(),
                    column: m.from.clone(),
                });
            }
            if m.switch.is_some() {
                return Err(ValidationError::SwitchRequiresFields {
                    flow: flow_name.to_string(),
                    column: m.from.clone(),
                });
            }
            Ok(ColumnConversionPlan {
                source: Some(DataType::Json),
                sink: DataType::Json,
                ctx: ConversionContext::passthrough(),
                switch: None,
            })
        })
        .collect()
}

/// Read source schema either from the (already-built) ctx or by direct
/// introspection. Used by `validate_flow`. We always go via
/// `describe_schema` here because at the point this runs the ctx is not
/// yet built — building ctx requires the final ReadSpec.columns, which
/// in turn requires expansion against the schema. The runner uses the
/// ctx-cached schema later on its hot path.
async fn fetch_source_schema(flow: &AssembledFlow) -> Result<Schema, ValidationError> {
    flow.source
        .describe_schema(&flow.config_read_spec.table)
        .await
        .map_err(|e| ValidationError::AccessFailed {
            component: "source:schema",
            name: flow.name.clone(),
            source: Box::new(e),
        })
}

async fn fetch_sink_schema(flow: &AssembledFlow) -> Result<Schema, ValidationError> {
    flow.sink
        .describe_schema(&flow.config_write_spec.table)
        .await
        .map_err(|e| ValidationError::AccessFailed {
            component: "sink:schema",
            name: flow.name.clone(),
            source: Box::new(e),
        })
}

/// Drive sampling-validation through the production [`FlowRunner`]
/// path with `dry_run = true`. The runner reuses
/// [`Source::sample`] in place of `read_batch`, threads `dry_run`
/// through to sink writes (parsed but not committed) and storage
/// cursor saves (skipped). The same pack → convert → write pipeline
/// the live tick exercises runs here, so any drift between sampling
/// and runtime would surface in this call.
///
/// `flow.config_read_spec.limit` is overridden to `size` so the sample
/// probe pulls the configured row count regardless of the user-declared
/// `batch-limit`. The runner's first tick rebuilds derived from the
/// post-override `config_read_spec`, so we don't need to mutate the
/// provided `derived` snapshot here.
async fn run_sampling_via_tick(
    flow: &AssembledFlow,
    derived: &DerivedPlans,
    size: usize,
) -> Result<(), ValidationError> {
    let mut sample_flow = flow.clone();
    sample_flow.config_read_spec.limit = size;
    let sample_derived = derived.clone();

    let state = FlowState::new(sample_flow, sample_derived);
    crate::flow::runner::FlowRunner::run_sample_probe(state)
        .await
        .map_err(|e| ValidationError::SamplingFailed {
            flow: flow.name.clone(),
            row_index: 0,
            field: "<sampling>".into(),
            source_type: DataType::Json,
            sink_type: DataType::Json,
            detail: e.to_string(),
        })?;
    Ok(())
}

async fn validate_flow(flow: AssembledFlow) -> Result<FlowState, ValidationError> {
    info!(flow = %flow.name, "validating flow");

    // ---- expansion ---------------------------------------------------
    // For wildcard / json-pack flows we need schemas before we can
    // finalise the ReadSpec / WriteSpec column lists. Direct-only flows
    // already have those filled in by `assemble`.
    let src_schemaless = flow.source.schemaless();
    let dst_schemaless = flow.sink.schemaless();
    let raw_passthrough_eligible = flow
        .rules
        .iter()
        .any(|r| matches!(r, ColumnMapping::Wildcard))
        && src_schemaless
        && dst_schemaless;
    let needs_schemas = flow.fields_check
        && !raw_passthrough_eligible
        && flow
            .rules
            .iter()
            .any(|r| matches!(r, ColumnMapping::Wildcard | ColumnMapping::Body { .. }));

    // Body / wildcard expansion against a schemaless source needs the
    // sample-derived schema — without it `expand` cannot enumerate the
    // source columns the body payload draws from. The describe_schema
    // hook on each schemaless connector samples its data plane (Mongo:
    // `$sample` + per-field type folding); fetching it here lets the
    // body branch see a `SchemalessWithSample` Schema rather than the
    // sample-less `Schemaless` arm.
    let needs_source_sample = needs_schemas
        && src_schemaless
        && flow
            .rules
            .iter()
            .any(|r| matches!(r, ColumnMapping::Body { .. }));

    // Each backend call below scopes its own per-component permit —
    // hold for the duration of that I/O, release before the next call.
    // No call site ever holds two permits at once, so deadlock is
    // structurally impossible (no canonical lock order needed).
    let lock_handle = flow.lock_handle.clone();
    let acquire_err = |e: RuntimeError, comp: &'static str| ValidationError::AccessFailed {
        component: comp,
        name: flow.name.clone(),
        source: Box::new(e),
    };

    let (src_schema, dst_schema): (Option<Schema>, Option<Schema>) = if needs_schemas {
        let src = if src_schemaless {
            if needs_source_sample {
                let _g = lock_handle
                    .acquire_source()
                    .await
                    .map_err(|e| acquire_err(e, "source-semaphore"))?;
                Some(fetch_source_schema(&flow).await?)
            } else {
                None
            }
        } else {
            let _g = lock_handle
                .acquire_source()
                .await
                .map_err(|e| acquire_err(e, "source-semaphore"))?;
            Some(fetch_source_schema(&flow).await?)
        };
        let dst = if dst_schemaless {
            None
        } else {
            let _g = lock_handle
                .acquire_sink()
                .await
                .map_err(|e| acquire_err(e, "sink-semaphore"))?;
            Some(fetch_sink_schema(&flow).await?)
        };
        // Wildcard / json-pack against a fully schemaless source where
        // the sink does have a schema is allowed — we use the sink as
        // the universe (mongo source → pg sink direction). The
        // wildcard-only-no-schema case is caught by `expand`.
        (src, dst)
    } else {
        (None, None)
    };

    // Collapse `(schemaless flag, optional schema)` into a `Schema`
    // whose `SchemaKind` discriminates the three expansion arms. A
    // schemaless source whose sample schema we just fetched (for body
    // expansion) collapses to `SchemalessWithSample` so `expand` sees
    // the carried fields.
    let src_state: Schema = match (src_schemaless, src_schema.clone()) {
        (false, Some(s)) => s,
        (true, Some(s)) => Schema::schemaless_with_sample(s.fields().to_vec()),
        (true, None) | (false, None) => Schema::schemaless(),
    };
    let dst_state: Schema = match (dst_schemaless, dst_schema.clone()) {
        (false, Some(s)) => s,
        (true, _) | (false, None) => Schema::schemaless(),
    };

    let expanded = mapping::expand(
        &flow.rules,
        &src_state,
        &dst_state,
        src_schemaless,
        dst_schemaless,
        &flow.name,
    )?;

    // Schemaless-both `["*"]` raw-passthrough invariants: the lowered
    // shape is direct=[], body=Some({_root}), so cursor.fields and
    // conflict.key cannot resolve to any direct column. Detect that
    // shape (no direct columns + a single body target named `_root`)
    // and reject explicit cursor / conflict configuration up front.
    let is_root_passthrough = expanded.direct.is_empty()
        && expanded.body.as_ref().is_some_and(|b| {
            b.targets.len() == 1 && b.targets[0] == crate::mapping::ROOT_BODY_TARGET
        });
    if is_root_passthrough {
        if !flow.config_read_spec.cursor_fields.is_empty() {
            return Err(ValidationError::CursorRequiresExplicitFields {
                flow: flow.name.clone(),
            });
        }
        if let Some(conflict) = &flow.config_write_spec.conflict
            && let Some(first) = conflict.key.first()
        {
            return Err(ValidationError::ConflictKeyNotInMapping {
                flow: flow.name.clone(),
                key: first.clone(),
            });
        }
    }

    // Build probe `ReadSpec` / `WriteSpec` from the post-expansion
    // column lists + the schema-independent config spec halves. These
    // mirror the shape the runner ultimately consumes via `DerivedPlans`
    // and feed the access/insert probes below.
    let probe_read_spec = ReadSpec {
        columns: expanded.read_columns(),
        table: flow.config_read_spec.table.clone(),
        cursor_fields: flow.config_read_spec.cursor_fields.clone(),
        cursor_order: flow.config_read_spec.cursor_order,
        limit: flow.config_read_spec.limit,
        source_options: flow.config_read_spec.source_options.clone(),
        needs_body: expanded.body.is_some(),
    };
    let probe_write_spec = WriteSpec {
        columns: expanded.write_columns(),
        table: flow.config_write_spec.table.clone(),
        conflict: flow.config_write_spec.conflict.clone(),
    };

    // ---- access probes -----------------------------------------------
    // Each probe under its own component permit, taken right before
    // and released right after — no holding while sibling probes run.
    if flow.access_check {
        {
            let _g = lock_handle
                .acquire_storage()
                .await
                .map_err(|e| acquire_err(e, "storage-semaphore"))?;
            retry_transient(|| flow.storage.validate_access())
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "storage",
                    name: flow.name.clone(),
                    source: Box::new(e),
                })?;
        }
        {
            let _g = lock_handle
                .acquire_source()
                .await
                .map_err(|e| acquire_err(e, "source-semaphore"))?;
            retry_transient(|| flow.source.validate_access(&probe_read_spec))
                .await
                .map_err(|e| ValidationError::AccessFailed {
                    component: "source",
                    name: flow.name.clone(),
                    source: Box::new(e),
                })?;
        }
    } else {
        info!(flow = %flow.name, "validation.access disabled — skipping source/storage probes");
    }
    if flow.inserts_check {
        let _g = lock_handle
            .acquire_sink()
            .await
            .map_err(|e| acquire_err(e, "sink-semaphore"))?;
        retry_transient(|| flow.sink.validate_access(&probe_write_spec))
            .await
            .map_err(|e| ValidationError::AccessFailed {
                component: "sink",
                name: flow.name.clone(),
                source: Box::new(e),
            })?;
        if flow.source.emits_deletes()
            && flow.config_write_spec.conflict.is_some()
            && flow.sink.supports_deletes()
        {
            retry_transient(|| flow.sink.validate_delete_access(&probe_write_spec))
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

    // ---- post-expansion checks + derived-plan build ----------------
    // For `fields_check = true` we need both schemas to run the
    // validation-only checks (matrix, cursor type, JSON-pack target
    // type, subset checks) and then delegate plan construction to
    // `build_derived_plans` — the single source of truth shared with
    // the runner-side rebuild path.
    let derived = if flow.fields_check {
        let src_schema_full = match src_schema {
            Some(s) => s,
            None => {
                let _g = lock_handle
                    .acquire_source()
                    .await
                    .map_err(|e| acquire_err(e, "source-semaphore"))?;
                fetch_source_schema(&flow).await?
            }
        };
        let dst_schema_full = match dst_schema {
            Some(s) => s,
            None if flow.sink.schemaless() => {
                // Schemaless sink — derived inside `build_derived_plans`
                // from the source schema. We still need a dst schema
                // here for the JSON-pack target type check + subset
                // checks below; build the same one the helper would.
                Schema::default()
            }
            None => {
                let _g = lock_handle
                    .acquire_sink()
                    .await
                    .map_err(|e| acquire_err(e, "sink-semaphore"))?;
                fetch_sink_schema(&flow).await?
            }
        };

        // Cursor presence + cursor-typability.
        checks::check_cursor(
            &flow.name,
            &src_schema_full,
            &flow.config_read_spec.cursor_fields,
        )?;
        for field_name in &flow.config_read_spec.cursor_fields {
            if let Some(field) = src_schema_full.find(field_name)
                && !field.data_type.cursor_compatible()
            {
                return Err(ValidationError::CursorTypeUnsupported {
                    field: field_name.clone(),
                    data_type: field.data_type.clone(),
                });
            }
        }

        if !is_root_passthrough {
            // Schemaless sinks bypass the matrix — no real dst schema
            // to narrow against. Schemaless sources bypass the matrix
            // narrowing / nullability check too — the sampled
            // "source type" is a hypothesis, not a contract, and the
            // Transform compiler emits dynamic-dispatch Convert ops
            // keyed off the sink type only.
            if !flow.sink.schemaless() && !flow.source.schemaless() {
                checks::check_mapping(&src_schema_full, &dst_schema_full, &expanded.direct)?;
            } else if flow.source.schemaless() && !src_schema_full.fields().is_empty() {
                // Schemaless source with a non-empty sample — still
                // catch typo'd `from` against the sampled fields.
                // Nullability is NOT enforced (sampling is non-
                // exhaustive). An empty sample skips this check
                // because we have no information either way.
                checks::check_mapping_sources_exist(&src_schema_full, &expanded.direct)?;
            }

            // Body / wildcard-pack target type check — every body
            // target sink column must accept the source's body
            // `DataType` (Json for relational sources, Json for
            // mongo too — the Custom `BsonObjectValue` wrapping
            // happens at apply time, validation operates on the
            // canonical pivot). Schemaless sinks (Mongo) bypass
            // this branch (no real dst schema).
            if !flow.sink.schemaless() {
                let carrier = flow.source.body_data_type();
                for target in expanded.body.as_ref().into_iter().flat_map(|p| &p.targets) {
                    let sink_field = dst_schema_full.find(target).ok_or_else(|| {
                        ValidationError::MissingField {
                            side: "sink",
                            field: target.clone(),
                        }
                    })?;
                    if !crate::types::matrix::is_compatible(
                        carrier.clone(),
                        sink_field.data_type.clone(),
                    ) {
                        return Err(ValidationError::IncompatibleTypes {
                            field: target.clone(),
                            from: carrier.clone(),
                            to: sink_field.data_type.clone(),
                            source: crate::error::TypeError::UnsupportedCast {
                                from: carrier.clone(),
                                to: sink_field.data_type.clone(),
                            },
                        });
                    }
                }
            }

            // cursor.fields ⊆ direct.from.
            let direct_from: ahash::AHashSet<&str> =
                expanded.direct.iter().map(|d| d.from.as_str()).collect();
            for cf in &flow.config_read_spec.cursor_fields {
                if !direct_from.contains(cf.as_str()) {
                    return Err(ValidationError::MissingCursorField {
                        flow: flow.name.clone(),
                        field: cf.clone(),
                    });
                }
            }
            // conflict.key ⊆ direct.to (or any body target).
            if let Some(conflict) = &flow.config_write_spec.conflict {
                let direct_to: ahash::AHashSet<&str> =
                    expanded.direct.iter().map(|d| d.to.as_str()).collect();
                let pack_targets: ahash::AHashSet<&str> = expanded
                    .body
                    .as_ref()
                    .into_iter()
                    .flat_map(|p| p.targets.iter())
                    .map(|s| s.as_str())
                    .collect();
                for k in &conflict.key {
                    let in_direct = direct_to.contains(k.as_str());
                    let in_body = pack_targets.contains(k.as_str());
                    if !in_direct && !in_body {
                        return Err(ValidationError::ConflictKeyNotInMapping {
                            flow: flow.name.clone(),
                            key: k.clone(),
                        });
                    }
                }
            }
        }

        // batch_limit × column_count ≤ 60_000 (post-expansion).
        let pack_cols = expanded.body.as_ref().map(|p| p.targets.len()).unwrap_or(0);
        let total_cols = expanded.direct.len() + pack_cols;
        let product = flow.config_read_spec.limit.saturating_mul(total_cols);
        if product > 60_000 {
            return Err(ValidationError::AccessFailed {
                component: "flow",
                name: flow.name.clone(),
                source: Box::new(RuntimeError::Other(format!(
                    "flow {:?}: batch-limit ({}) × mapping cols ({}) = {} exceeds 60000",
                    flow.name, flow.config_read_spec.limit, total_cols, product
                ))),
            });
        }

        // Single source of truth for plan construction. Pass
        // `dst_schema = None` for schemaless sinks so the helper
        // synthesises the same shape the runner-side rebuild does.
        // We already paid the `mapping::expand` cost above for the
        // raw-passthrough invariants and spec-column rewrite, so go
        // through the `_from_expanded` variant to avoid double-expand.
        let dst_for_build = if flow.sink.schemaless() {
            None
        } else {
            Some(&dst_schema_full)
        };
        let built = crate::model::flow_state::build_derived_plans_from_expanded(
            &flow,
            &expanded,
            Some(&src_schema_full),
            dst_for_build,
            dst_schemaless,
        )?;

        // Sink-aware compatibility check: compare every post-transform
        // output `DataType` against the corresponding sink column. This
        // replaces the source-vs-sink matrix branch that used to live
        // in `checks::check_mapping` and accepts cross-family flows
        // (e.g. `Bool → Text` via `Switch`) that the lossless matrix
        // would have refused on the raw source type. Schemaless sinks
        // short-circuit inside the validator.
        let sink_column_names: Vec<String> = expanded
            .direct
            .iter()
            .map(|d| d.to.clone())
            .chain(
                expanded
                    .body
                    .as_ref()
                    .into_iter()
                    .flat_map(|b| b.targets.iter().cloned()),
            )
            .collect();
        let source_body_dt = flow.source.body_data_type();
        let validator = CompatibilityValidator::new(
            &flow.name,
            &built.transform,
            &src_schema_full,
            &source_body_dt,
        );
        validator.validate(&dst_schema_full, &sink_column_names, flow.sink.schemaless())?;
        built
    } else {
        info!(
            flow = %flow.name,
            "validation.fields disabled — skipping schema introspection; conversions are passthrough"
        );
        // No schemas available — build passthrough plans inline so we
        // skip default_literal parsing (the helper would need a sink
        // type to parse defaults against).
        let conversions = passthrough_plans(&flow.name, &expanded.direct)?;
        let read_columns = expanded.read_columns();
        let read_spec = ReadSpec {
            columns: read_columns.clone(),
            table: flow.config_read_spec.table.clone(),
            cursor_fields: flow.config_read_spec.cursor_fields.clone(),
            cursor_order: flow.config_read_spec.cursor_order,
            limit: flow.config_read_spec.limit,
            source_options: flow.config_read_spec.source_options.clone(),
            needs_body: expanded.body.is_some(),
        };
        let write_spec = WriteSpec {
            columns: expanded.write_columns(),
            table: flow.config_write_spec.table.clone(),
            conflict: flow.config_write_spec.conflict.clone(),
        };
        let body_data_type = flow.source.body_data_type();
        let transform = crate::transform::compile_to_transform(
            &expanded,
            body_data_type,
            &conversions,
            &[],
            &read_columns,
            flow.source.schemaless(),
            // `passthrough_plans` already rejected any compute column, so
            // there are no lowerings or compute context on this path.
            &[],
            None,
        )?;
        DerivedPlans {
            read_spec,
            write_spec,
            transform,
        }
    };

    if let SamplingConfig::Enabled { size } = flow.sampling {
        // Sampling probe runs through `runner::tick(dry_run = true)`,
        // which acquires the per-phase permits itself. No outer lock
        // to drop here.
        run_sampling_via_tick(&flow, &derived, size).await?;
    }

    info!(flow = %flow.name, "flow validated");
    Ok(FlowState::new(flow, derived))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::any::Any;
    use std::sync::Arc;
    use std::time::Duration;

    use crate::config::model::CursorOrder;
    use crate::config::validation::SamplingConfig;
    use crate::error::ValidationError;
    use crate::flow::test_utils::{default_source_mock, raw_passthrough_source_mock};
    use crate::mapping::ColumnMapping;
    use crate::model::{AssembledFlow, ConfigReadSpec, ConfigWriteSpec};
    use crate::traits::{MockSink, MockSource, MockStorage};
    use crate::types::DataType;

    use super::*;

    fn flow_with(
        source: MockSource,
        sink: MockSink,
        storage: MockStorage,
        rules: Vec<ColumnMapping>,
        fields_check: bool,
    ) -> AssembledFlow {
        AssembledFlow {
            name: "test".into(),
            source: Arc::new(source),
            sink: Arc::new(sink),
            storage: Arc::new(storage),
            rules,
            config_read_spec: ConfigReadSpec {
                table: "t".into(),
                cursor_fields: vec!["a".into()],
                cursor_order: CursorOrder::Asc,
                limit: 1,
                source_options: toml::Table::new(),
            },
            config_write_spec: ConfigWriteSpec {
                table: "t".into(),
                conflict: None,
            },
            interval: Duration::from_millis(10),
            jitter: Duration::ZERO,
            query_timeout: Duration::from_secs(5),
            sampling: SamplingConfig::Disabled,
            access_check: false,
            fields_check,
            inserts_check: false,
            cursor_persistence: crate::model::CursorPersistence::ColumnCursor,
            lock_handle: {
                let mut m = crate::util::ConcurrencyManager::new();
                m.register_source("test-source", u32::MAX);
                m.register_sink("test-sink", u32::MAX);
                m.register_storage("test-storage", u32::MAX);
                m.handle(
                    "test-source",
                    "test-sink",
                    "test-storage",
                    &mut air_elt_monitoring::MonitoringManager::disabled(),
                )
            },
            recorder: air_elt_monitoring::FlowRecorder::disabled(),
            expr_context: Arc::new(air_elt_expr_runtime::ExpressionContext::create(
                Arc::new(air_elt_expr_funcs::FunctionRegistry::with_builtins()),
                std::path::Path::new("/tmp"),
            )),
        }
    }

    fn rule_direct(from: &str, to: &str) -> ColumnMapping {
        ColumnMapping::Direct {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default_literal: None,
        }
    }

    fn rule_direct_with_default(from: &str, to: &str, default: toml::Value) -> ColumnMapping {
        ColumnMapping::Direct {
            from: from.into(),
            to: to.into(),
            truncate: false,
            default_literal: Some(default),
        }
    }

    fn rule_switch(from: &str, to: &str) -> ColumnMapping {
        ColumnMapping::Switch {
            from: from.into(),
            to: to.into(),
            truncate: false,
            cases: vec![crate::mapping::column::SwitchCase {
                key: "x".into(),
                value: toml::Value::String("y".into()),
            }],
            default_literal: None,
        }
    }

    fn rule_compute(to: &str, expr: &str) -> ColumnMapping {
        ColumnMapping::Compute {
            to: to.into(),
            expr_source: expr.into(),
            truncate: false,
            default_literal: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fields_check_false_skips_describe_schema() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_describe_schema().times(0);
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_describe_schema().times(0);
        let storage = MockStorage::new();

        let rules = vec![rule_direct("a", "a")];
        let flow = flow_with(source, sink, storage, rules, false);

        let state = validate_flow(flow).await.unwrap();
        let derived = state.derived();
        assert_eq!(derived.transform.cols.len(), 1);
    }

    fn flow_with_inserts_and_conflict(
        source: MockSource,
        sink: MockSink,
        storage: MockStorage,
        conflict: Option<crate::config::conflict::ConflictConfig>,
    ) -> AssembledFlow {
        let rules = vec![rule_direct("a", "a")];
        let mut flow = flow_with(source, sink, storage, rules, false);
        flow.inserts_check = true;
        flow.config_write_spec.conflict = conflict;
        flow
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_invoked_when_source_emits_deletes_and_conflict_set() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_emits_deletes().return_const(true);
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(true);
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
        validate_flow(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_skipped_when_source_does_not_emit_deletes() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_emits_deletes().return_const(false);
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(true);
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
        validate_flow(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_skipped_without_conflict_block() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_emits_deletes().return_const(true);
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(true);
        sink.expect_validate_access().times(1).returning(|_| Ok(()));
        sink.expect_validate_delete_access().times(0);
        let storage = MockStorage::new();
        let flow = flow_with_inserts_and_conflict(source, sink, storage, None);
        validate_flow(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_probe_skipped_when_sink_does_not_support_deletes() {
        // Source emits deletes AND a conflict block is set, but the
        // sink declares `supports_deletes() == false`. The runner will
        // drop delete rows before write_batch, so no delete-access
        // probe should fire on the sink.
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_emits_deletes().return_const(true);
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(false);
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
        validate_flow(flow).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fields_check_false_with_default_rejected() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_describe_schema().times(0);
        let mut sink = MockSink::new();
        sink.expect_describe_schema().times(0);
        sink.expect_schemaless().return_const(false);
        let storage = MockStorage::new();

        let rules = vec![rule_direct_with_default(
            "a",
            "a",
            toml::Value::String("x".into()),
        )];
        let flow = flow_with(source, sink, storage, rules, false);

        let res = validate_flow(flow).await;
        let err = match res {
            Ok(_) => panic!("expected DefaultRequiresFields, got Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ValidationError::DefaultRequiresFields { .. }),
            "expected DefaultRequiresFields, got {err:?}"
        );
    }

    /// Mirror of the `DefaultRequiresFields` test for the parallel
    /// `switch` rejection branch (`passthrough_plans`). Without schema
    /// introspection the switch's source/sink types can't be resolved,
    /// so the mapping must be rejected at validate time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fields_check_false_with_switch_rejected() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_describe_schema().times(0);
        let mut sink = MockSink::new();
        sink.expect_describe_schema().times(0);
        sink.expect_schemaless().return_const(false);
        let storage = MockStorage::new();

        let rules = vec![rule_switch("a", "a")];
        let flow = flow_with(source, sink, storage, rules, false);

        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(err, ValidationError::SwitchRequiresFields { .. }),
            "expected SwitchRequiresFields, got {err:?}"
        );
    }

    /// Mirror of the `DefaultRequiresFields` / `SwitchRequiresFields` tests
    /// for the compute branch (`passthrough_plans`). A compute script needs
    /// the sink type to type-check, so without schema introspection
    /// (`fields = false`) the mapping is rejected at validate time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fields_check_false_with_compute_rejected() {
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source.expect_describe_schema().times(0);
        let mut sink = MockSink::new();
        sink.expect_describe_schema().times(0);
        sink.expect_schemaless().return_const(false);
        let storage = MockStorage::new();

        let rules = vec![rule_compute("a", "1 + 2")];
        let flow = flow_with(source, sink, storage, rules, false);

        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(err, ValidationError::ComputeRequiresFields { .. }),
            "expected ComputeRequiresFields, got {err:?}"
        );
    }

    // ---- Cursor type guard ---------------------------------------

    use crate::model::{Field, Schema};
    use crate::types::convert::ConvertError;
    use crate::types::convert::context::ConversionContext;
    use crate::types::dynamic::DynType;
    use crate::types::value::Value;

    #[derive(Debug)]
    struct NonCursorCustom;

    impl DynType for NonCursorCustom {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn kind(&self) -> &str {
            "test.non_cursor"
        }
        fn can_convert_to(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn can_construct_from(&self, _t: &DataType, _trunc: bool) -> bool {
            false
        }
        fn convert(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn construct(
            &self,
            _v: Value,
            _t: &DataType,
            _ctx: &ConversionContext,
        ) -> Result<Value, ConvertError> {
            unimplemented!()
        }
        fn clone_box(&self) -> Box<dyn DynType> {
            Box::new(NonCursorCustom)
        }
    }

    fn flow_for_cursor_test(cursor_field_type: DataType) -> AssembledFlow {
        let src_schema = Schema::new(vec![Field {
            name: "a".into(),
            data_type: cursor_field_type,
            nullable: false,
        }]);
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source
            .expect_describe_schema()
            .returning(move |_| Ok(src_schema.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(true);
        let storage = MockStorage::new();

        let rules = vec![rule_direct("a", "a")];
        flow_with(source, sink, storage, rules, true)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_guard_rejects_json_field() {
        let flow = flow_for_cursor_test(DataType::Json);
        let res = validate_flow(flow).await;
        let err = match res {
            Ok(_) => panic!("expected CursorTypeUnsupported, got Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ValidationError::CursorTypeUnsupported { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_guard_rejects_xml_field() {
        let flow = flow_for_cursor_test(DataType::Xml);
        let res = validate_flow(flow).await;
        let err = match res {
            Ok(_) => panic!("expected CursorTypeUnsupported, got Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ValidationError::CursorTypeUnsupported { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_guard_rejects_union_field() {
        let flow = flow_for_cursor_test(DataType::Union(vec![DataType::Int32, DataType::Int64]));
        let res = validate_flow(flow).await;
        let err = match res {
            Ok(_) => panic!("expected CursorTypeUnsupported, got Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ValidationError::CursorTypeUnsupported { .. }),
            "got {err:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_guard_accepts_int64_field() {
        let flow = flow_for_cursor_test(DataType::Int64);
        validate_flow(flow).await.expect("Int64 cursor should pass");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cursor_guard_rejects_custom_without_cursor_compatible() {
        let flow = flow_for_cursor_test(DataType::Custom(Box::new(NonCursorCustom)));
        let res = validate_flow(flow).await;
        let err = match res {
            Ok(_) => panic!("expected CursorTypeUnsupported, got Ok"),
            Err(e) => e,
        };
        assert!(
            matches!(err, ValidationError::CursorTypeUnsupported { .. }),
            "got {err:?}"
        );
    }

    // ---- Wildcard / json-pack pipeline integration ----------------

    /// `cursor.fields=["id"]` + expanded universe lacking `id` →
    /// `MissingCursorField` post-expansion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn wildcard_cursor_missing_after_expansion() {
        let src_schema = Schema::new(vec![Field {
            name: "name".into(),
            data_type: DataType::Text { size: None },
            nullable: false,
        }]);
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        source
            .expect_describe_schema()
            .returning(move |_| Ok(src_schema.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(true);
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![ColumnMapping::Wildcard], true);
        flow.config_read_spec.cursor_fields = vec!["id".into()];
        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(&err, ValidationError::MissingCursorField { field, .. } if field == "id"),
            "got {err:?}"
        );
    }

    /// `batch_limit=200 × 301 cols → 60_000` violation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn batch_limit_times_cols_post_expansion() {
        let fields: Vec<Field> = (0..301)
            .map(|i| Field {
                name: format!("c{i}"),
                data_type: DataType::Int32,
                nullable: false,
            })
            .collect();
        let schema = Schema::new(fields);
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        let cloned = schema.clone();
        source
            .expect_describe_schema()
            .returning(move |_| Ok(cloned.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(true);
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![ColumnMapping::Wildcard], true);
        flow.config_read_spec.cursor_fields = vec!["c0".into()];
        flow.config_read_spec.limit = 200;
        let err = validate_flow(flow).await.unwrap_err();
        // 200 * 301 = 60_200 > 60_000.
        let msg = err.to_string();
        assert!(msg.contains("60200"), "got {msg}");
    }

    /// Body target column whose type the matrix refuses for the body
    /// `DataType::Json`. Pre-Step-6 this surfaced as a dedicated
    /// `JsonPackTargetNotJson` variant; the body type check
    /// now runs through `matrix::is_compatible(Json, sink_target)` and
    /// surfaces as the standard `IncompatibleTypes`. Pick a sink type
    /// the matrix actively rejects (`Int32`) — the matrix admits
    /// `Json → Text { size: None }` as a widening, so a Text sink is no
    /// longer a counter-example here.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn body_target_not_json() {
        let src_schema = Schema::new(vec![Field {
            name: "a".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let dst_schema = Schema::new(vec![Field {
            name: "body".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        let s = src_schema.clone();
        source
            .expect_describe_schema()
            .returning(move |_| Ok(s.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        let d = dst_schema.clone();
        sink.expect_describe_schema()
            .returning(move |_| Ok(d.clone()));
        let storage = MockStorage::new();
        let mut flow = flow_with(
            source,
            sink,
            storage,
            vec![ColumnMapping::Body { to: "body".into() }],
            true,
        );
        flow.config_read_spec.cursor_fields = vec!["a".into()];
        let err = validate_flow(flow).await.unwrap_err();
        // Body target type check now flows through the standard
        // matrix: `Json → Text` is rejected as `IncompatibleTypes`.
        assert!(
            matches!(
                err,
                ValidationError::IncompatibleTypes { ref field, .. } if field == "body"
            ),
            "got {err:?}"
        );
    }

    /// Raw passthrough + cursor.fields → error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raw_passthrough_with_cursor_rejected() {
        let mut source = raw_passthrough_source_mock();
        source.expect_name().return_const("src".to_string());
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(true);
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![ColumnMapping::Wildcard], true);
        flow.config_read_spec.cursor_fields = vec!["id".into()];
        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(err, ValidationError::CursorRequiresExplicitFields { .. }),
            "got {err:?}"
        );
    }

    /// Raw passthrough + conflict.key → error.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn raw_passthrough_with_conflict_rejected() {
        let mut source = raw_passthrough_source_mock();
        source.expect_name().return_const("src".to_string());
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(true);
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![ColumnMapping::Wildcard], true);
        flow.config_read_spec.cursor_fields = vec![];
        flow.config_write_spec.conflict = Some(crate::config::conflict::ConflictConfig {
            key: vec!["id".into()],
            strategy: crate::config::conflict::ConflictStrategy::Overwrite,
        });
        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(&err, ValidationError::ConflictKeyNotInMapping { key, .. } if key == "id"),
            "got {err:?}"
        );
    }

    /// Explicit long-form mapping where source column is missing →
    /// today's `MissingField` (not the wildcard null-inject).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explicit_missing_source_uses_missing_field() {
        let src_schema = Schema::new(vec![Field {
            name: "a".into(),
            data_type: DataType::Int32,
            nullable: false,
        }]);
        let dst_schema = Schema::new(vec![Field {
            name: "b".into(),
            data_type: DataType::Int32,
            nullable: true,
        }]);
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        let s = src_schema.clone();
        source
            .expect_describe_schema()
            .returning(move |_| Ok(s.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        let d = dst_schema.clone();
        sink.expect_describe_schema()
            .returning(move |_| Ok(d.clone()));
        let storage = MockStorage::new();
        let mut flow = flow_with(
            source,
            sink,
            storage,
            vec![rule_direct("missing", "b")],
            true,
        );
        flow.config_read_spec.cursor_fields = vec![];
        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(&err, ValidationError::MissingField { side: "source", field, .. } if field == "missing"),
            "got {err:?}"
        );
    }

    // ---- Schemaless source — sample is non-authoritative -----------

    /// A schemaless source whose sample claims `Int64` but whose
    /// mapping targets a sink column of type `Int32` must validate
    /// successfully — the matrix narrowing check is implicit-off for
    /// schemaless sources (the sampled "source type" is a hypothesis,
    /// not a contract). The companion runtime test
    /// `schemaless_flow_accepts_cross_batch_value_drift` proves the
    /// per-cell dispatch handles the actual variant at apply time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn schemaless_source_skips_matrix_narrowing_check() {
        let src_schema = Schema::new(vec![Field {
            name: "n".into(),
            data_type: DataType::Int64,
            nullable: true,
        }]);
        let dst_schema = Schema::new(vec![Field {
            name: "n".into(),
            data_type: DataType::Int32,
            nullable: true,
        }]);
        let mut source = raw_passthrough_source_mock();
        source.expect_name().return_const("src".to_string());
        let s = src_schema.clone();
        source
            .expect_describe_schema()
            .returning(move |_| Ok(s.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        let d = dst_schema.clone();
        sink.expect_describe_schema()
            .returning(move |_| Ok(d.clone()));
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![rule_direct("n", "n")], true);
        flow.config_read_spec.cursor_fields = vec!["n".into()];
        let state = validate_flow(flow).await.expect("schemaless validates");
        // The compiled transform must emit a dynamic-source `Convert`
        // (`plan.source = None`), not a bare `Take` or a static
        // `Convert` — that's the whole point.
        match &state.derived().transform.cols[0] {
            crate::transform::TransformOp::Convert { plan, .. } => {
                assert!(
                    plan.source.is_none(),
                    "schemaless source must emit dynamic-source Convert"
                );
            }
            other => panic!("expected dynamic Convert, got {other:?}"),
        }
    }

    /// Companion: a typed source whose sample is nullable feeding a
    /// NOT NULL sink without a `default` still triggers
    /// `NullabilityMismatch` — confirms the schemaless skip is scoped
    /// to the schemaless flag and does not silently weaken the typed-
    /// source `check_mapping` branch.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn typed_source_keeps_nullability_check() {
        let src_schema = Schema::new(vec![Field {
            name: "n".into(),
            data_type: DataType::Int64,
            nullable: true,
        }]);
        let dst_schema = Schema::new(vec![Field {
            name: "n".into(),
            data_type: DataType::Int64,
            nullable: false,
        }]);
        let mut source = default_source_mock();
        source.expect_name().return_const("src".to_string());
        let s = src_schema.clone();
        source
            .expect_describe_schema()
            .returning(move |_| Ok(s.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        let d = dst_schema.clone();
        sink.expect_describe_schema()
            .returning(move |_| Ok(d.clone()));
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![rule_direct("n", "n")], true);
        flow.config_read_spec.cursor_fields = vec!["n".into()];
        let err = validate_flow(flow).await.unwrap_err();
        assert!(
            matches!(err, ValidationError::NullabilityMismatch { .. }),
            "typed source must still flag nullable→NOT-NULL, got {err:?}"
        );
    }

    /// And the schemaless companion of the above: the same
    /// nullable-source → NOT-NULL-sink shape is permitted under a
    /// schemaless source because the sampled `nullable` flag is non-
    /// authoritative (a sample can never prove NOT NULL or refute it
    /// authoritatively). The Transform compiler emits a dynamic-source
    /// `Convert` (`plan.source = None`) and the runtime's null-handling
    /// path applies a default if any
    /// was declared — there is no validation-time signal to fire on.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn schemaless_source_skips_nullability_check() {
        let src_schema = Schema::new(vec![Field {
            name: "n".into(),
            data_type: DataType::Int64,
            nullable: true,
        }]);
        let dst_schema = Schema::new(vec![Field {
            name: "n".into(),
            data_type: DataType::Int64,
            nullable: false,
        }]);
        let mut source = raw_passthrough_source_mock();
        source.expect_name().return_const("src".to_string());
        let s = src_schema.clone();
        source
            .expect_describe_schema()
            .returning(move |_| Ok(s.clone()));
        let mut sink = MockSink::new();
        sink.expect_schemaless().return_const(false);
        let d = dst_schema.clone();
        sink.expect_describe_schema()
            .returning(move |_| Ok(d.clone()));
        let storage = MockStorage::new();
        let mut flow = flow_with(source, sink, storage, vec![rule_direct("n", "n")], true);
        flow.config_read_spec.cursor_fields = vec!["n".into()];
        validate_flow(flow)
            .await
            .expect("schemaless skips the nullability check");
    }
}
