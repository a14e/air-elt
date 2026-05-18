use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ahash::AHasher;
use tokio::sync::watch;
use tokio::time::{Instant, sleep, sleep_until};
use tracing::{debug, error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{
    Batch, CursorFieldValue, CursorPersistence, CursorState, FlowState, Schema, SinkCtx, SourceCtx,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Daemon,
    Once,
}

/// What the runner loop should do after a tick returns. `Continue` runs
/// the next tick immediately (the source produced a full batch and may
/// have more rows queued). `Idle` waits for the next grid point (the
/// source returned an empty batch). `Exit` ends the runner (Once mode
/// drain complete or shutdown signalled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TickOutcome {
    Continue,
    Idle,
    Exit,
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(3600);
const BACKOFF_MULTIPLIER: u32 = 4;

pub(crate) struct FlowRunner {
    flow: FlowState,
    mode: RunMode,
    shutdown: watch::Receiver<bool>,
    cursor: Option<CursorState>,
    backoff: Duration,
    source_ctx: Option<Arc<dyn SourceCtx>>,
    sink_ctx: Option<Arc<dyn SinkCtx>>,
}

impl FlowRunner {
    pub fn new(flow: FlowState, mode: RunMode, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            flow,
            mode,
            shutdown,
            cursor: None,
            backoff: BACKOFF_INITIAL,
            source_ctx: None,
            sink_ctx: None,
        }
    }

    /// Compute the deterministic per-flow startup offset:
    /// `hash(flow.name) % jitter` (millisecond resolution).
    ///
    /// **Why deterministic, not random.** A name-hashed offset is
    /// reproducible across restarts — a flow that always lags at :23
    /// stays at :23, which makes lag debugging traceable to a single
    /// flow rather than a random shuffle. Operators can also reason
    /// about steady-state phase without re-rolling on every redeploy.
    ///
    /// `ahash::AHasher` is the workspace's standard hasher (already a
    /// `[workspace.dependencies]` member used by `AHashMap`); we avoid
    /// pulling in a separate hash crate just for this.
    fn jitter_offset(&self) -> Duration {
        let jitter = self.flow.jitter;
        if jitter.is_zero() {
            return Duration::ZERO;
        }
        let mut hasher = AHasher::default();
        self.flow.name.hash(&mut hasher);
        let hash = hasher.finish();
        // Operate in milliseconds: sub-millisecond precision is well
        // below the runner's tick latency and lets us keep arithmetic
        // in `u64` even for multi-hour jitter ceilings.
        let jitter_millis = jitter.as_millis().max(1) as u64;
        let offset_millis = hash % jitter_millis;
        Duration::from_millis(offset_millis)
    }

    pub async fn run(mut self) -> RuntimeResult<()> {
        // Deterministic startup jitter — shifts the first-tick schedule
        // grid by `hash(flow.name) % jitter` so a fleet of flows that
        // share the same `interval` doesn't pile up on the same
        // second-boundary. Subsequent ticks proceed at `interval`
        // cadence as before — this is a one-shot offset, not a per-tick
        // walk. **Honoured only in Daemon mode.** `RunMode::Once` is
        // for drain-and-exit (CLI e2e + `--once` one-shots); paying a
        // multi-minute jitter delay there would look like a hung
        // command. `jitter = 0s` also collapses to no sleep.
        if matches!(self.mode, RunMode::Daemon) {
            let offset = self.jitter_offset();
            if !offset.is_zero() {
                debug!(
                    flow = %self.flow.name,
                    offset_ms = offset.as_millis() as u64,
                    "applying startup jitter offset"
                );
                tokio::select! {
                    _ = sleep(offset) => {}
                    _ = self.shutdown.changed() => {
                        info!(flow = %self.flow.name, "shutdown during startup jitter");
                        return Ok(());
                    }
                }
            }
        }
        loop {
            match self.tick(false).await {
                Ok(TickOutcome::Exit) => {
                    return Ok(());
                }
                Ok(TickOutcome::Continue) => {
                    self.backoff = BACKOFF_INITIAL;
                }
                Ok(TickOutcome::Idle) => {
                    self.backoff = BACKOFF_INITIAL;
                    // Empty batch — wait for the next grid point. Permits
                    // were already released inside `tick`; this idle wait
                    // does NOT hold any backend semaphore so other flows
                    // sharing the pool can drain freely. See
                    // [`next_tick_instant`] for the scheduling design.
                    let now_since_epoch_ns = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos();
                    let next = next_tick_instant(
                        &self.flow.name,
                        self.flow.interval,
                        self.flow.jitter,
                        now_since_epoch_ns,
                        Instant::now(),
                    );
                    tokio::select! {
                        _ = sleep_until(next) => {}
                        _ = self.shutdown.changed() => {
                            return Ok(());
                        }
                    }
                }
                Err(e) => {
                    if matches!(self.mode, RunMode::Once) {
                        return Err(e);
                    }
                    // Drop the source/sink ctx Arcs on backend errors so
                    // the next tick rebuilds via `build_context` (which
                    // re-runs `describe_schema` and refreshes any cached
                    // plan). Per-row data errors (type / conversion /
                    // JSON encoding) are explicitly excluded — the ctx
                    // is fine, the row isn't. The drop happens **before**
                    // the backoff sleep so a shutdown during backoff
                    // cannot leak stale ctx state into a later run.
                    if should_drop_ctx_on(&e) {
                        debug!(
                            flow = %self.flow.name,
                            "dropping ctx + derived after backend error; next tick will rebuild"
                        );
                        self.source_ctx = None;
                        self.sink_ctx = None;
                    }
                    error!(
                        flow = %self.flow.name,
                        error = %e,
                        retry_in_secs = self.backoff.as_secs(),
                        "flow iteration failed; backing off"
                    );
                    tokio::select! {
                        _ = sleep(self.backoff) => {}
                        _ = self.shutdown.changed() => {
                            info!(flow = %self.flow.name, "shutdown during backoff");
                            return Ok(());
                        }
                    }
                    self.backoff = (self.backoff * BACKOFF_MULTIPLIER).min(BACKOFF_CAP);
                }
            }
        }
    }

    /// Run a single dry-run tick for sample-based validation. Builds a
    /// throwaway runner, drives one `tick(dry_run=true)`, and returns —
    /// the in-memory cursor advances and dies with the runner; storages
    /// and sinks skip persistence under `dry_run`. The validation
    /// pipeline is the sole caller.
    pub(crate) async fn run_sample_probe(flow: FlowState) -> RuntimeResult<()> {
        // `_keep_tx_alive` is named-bound (not `_`) so the sender lives
        // for the function scope — dropping it would close `shutdown_rx`
        // and trigger an immediate `shutdown.changed()` inside `tick`,
        // defeating the dry-run probe.
        let (_keep_tx_alive, shutdown_rx) = watch::channel(false);
        let mut runner = Self::new(flow, RunMode::Once, shutdown_rx);
        // Sampling-validation calls `tick(true)` directly. Startup
        // jitter lives in `run()` (Daemon-only now), not in `tick`,
        // so the probe never sleeps on it.
        runner.tick(true).await.map(|_outcome| ())
    }

    /// Ensure both source/sink ctxs are built and that `DerivedPlans`
    /// have been refreshed against the live ctx-side schemas after any
    /// ctx (re)build. `DerivedPlans` is always populated (constructed
    /// at validation time), so this only rebuilds derived when at least
    /// one ctx had to be (re)built — typically after a backend-error
    /// ctx drop. Schemas are pulled exclusively via `SchemaProvider`
    /// on the just-built ctxs.
    async fn ensure_built(&mut self) -> RuntimeResult<()> {
        if self.source_ctx.is_some() && self.sink_ctx.is_some() {
            return Ok(());
        }
        let rebuild_needed = self.source_ctx.is_none() || self.sink_ctx.is_none();
        if self.source_ctx.is_none() {
            // Source pool I/O — held only for this `build_context`.
            let _g = self.flow.lock_handle.acquire_source().await?;
            self.source_ctx = Some(
                self.flow
                    .source
                    .build_context(&self.flow.derived().read_spec)
                    .await?,
            );
        }
        if self.sink_ctx.is_none() {
            // Sink pool I/O — held only for this `build_context`.
            let _g = self.flow.lock_handle.acquire_sink().await?;
            self.sink_ctx = Some(
                self.flow
                    .sink
                    .build_context(&self.flow.derived().write_spec)
                    .await?,
            );
        }
        if rebuild_needed {
            let src_schemaless = self.flow.source.schemaless();
            let dst_schemaless = self.flow.sink.schemaless();
            let src_schema: Option<Schema> = self
                .source_ctx
                .as_ref()
                .and_then(|c| c.as_schema_provider())
                .map(|p| p.schema().clone());
            let dst_schema: Option<Schema> = self
                .sink_ctx
                .as_ref()
                .and_then(|c| c.as_schema_provider())
                .map(|p| p.schema().clone());

            // Collapse the (schemaless, schema) pair into a `Schema` whose
            // `SchemaKind` discriminates fixed / schemaless / schemaless-
            // with-sample. A schemaless connector with a populated ctx-side
            // schema is a *sample* — expose it via `Schema::schemaless_with_sample`
            // so wildcard-only schemaless-both flows still hit the
            // raw-passthrough fast path (which deliberately ignores the sample).
            //
            // An *empty* sample (mongo sink's `describe_schema` returns
            // `Schema::schemaless()` with no fields) carries no useful
            // info — collapse it to plain `Schemaless` so `build_derived_plans`
            // synthesises the dst schema from the source side instead of
            // trying to look up columns in the empty sample.
            let src_state: Schema = match (src_schemaless, src_schema) {
                (false, Some(s)) => s,
                (true, Some(s)) if !s.fields().is_empty() => {
                    Schema::schemaless_with_sample(s.fields().to_vec())
                }
                (true, _) => Schema::schemaless(),
                // Non-schemaless connector with no schema available — the
                // ctx provider returned None. Treat as Schemaless so expand
                // surfaces `WildcardWithoutSchema` rather than panicking;
                // this path is only reachable when the connector is
                // misbehaving.
                (false, None) => Schema::schemaless(),
            };
            let dst_state: Schema = match (dst_schemaless, dst_schema) {
                (false, Some(s)) => s,
                (true, Some(s)) if !s.fields().is_empty() => {
                    Schema::schemaless_with_sample(s.fields().to_vec())
                }
                (true, _) => Schema::schemaless(),
                (false, None) => Schema::schemaless(),
            };

            self.flow.rebuild_derived(&src_state, &dst_state)?;
        }
        Ok(())
    }

    /// Single iteration of the runner pipeline: load cursor → ensure
    /// ctx + derived plans → read (or sample) → pack → convert → write
    /// → save cursor / resume token.
    ///
    /// When `dry_run = true`, the runner uses [`Source::sample`] in
    /// place of `read_batch`, sinks are told to parse the batch without
    /// committing (server-side `WHERE FALSE` / `replaceOne($expr:false)`)
    /// and storages skip persistence. Used by the validation pipeline
    /// for sample-based pre-flight checks: the dry tick exercises the
    /// same pack → convert → write path the live tick does, so any
    /// drift between sampling and runtime would surface here.
    ///
    /// **Concurrency.** Each I/O unit inside `tick` scopes its own
    /// per-component permit: `ensure_built` takes source/sink permits
    /// for its respective `build_context` calls; cursor load/save and
    /// `with_storage_lock` use the storage permit; `read_batch` /
    /// `sample` use the source permit; `write_batch` uses the sink
    /// permit. No phase ever holds two permits at once, so no
    /// canonical ordering is needed and deadlock between flows is
    /// structurally impossible. The inter-tick idle sleep happens in
    /// `run` AFTER `tick` returns, so it never holds any permit.
    async fn tick(&mut self, dry_run: bool) -> Result<TickOutcome, RuntimeError> {
        // Build ctx + derived plans first so the cursor reload below
        // can resolve the expected per-field `DataType`s off the live
        // source schema (each cursor field's type drives the typed
        // `DataType::decode_cursor_json` dispatch inside the storage).
        // `ensure_built` takes source/sink permits internally, one at
        // a time, only for the build_context calls that need them.
        self.ensure_built().await?;

        if *self.shutdown.borrow() {
            info!(flow = %self.flow.name, "shutdown signalled");
            return Ok(TickOutcome::Exit);
        }

        if self.cursor.is_none() {
            // Cursor load under storage permit — held only for this
            // call. Released before we touch source/sink below.
            let _g = self.flow.lock_handle.acquire_storage().await?;
            self.cursor = match self.flow.cursor_persistence {
                CursorPersistence::ColumnCursor => {
                    let cursor_types = self.resolve_cursor_types()?;
                    let fut = self
                        .flow
                        .storage
                        .load_cursor(&self.flow.name, &cursor_types);
                    with_timeout(&self.flow, "load_cursor", fut, &mut self.shutdown).await?
                }
                CursorPersistence::ResumeToken => {
                    let fut = self.flow.storage.load_resume_token(&self.flow.name);
                    let token =
                        with_timeout(&self.flow, "load_resume_token", fut, &mut self.shutdown)
                            .await?;
                    token.map(|json| {
                        CursorState::new(vec![CursorFieldValue {
                            name: RESUME_TOKEN_FIELD.into(),
                            value: crate::types::Value::Json(json),
                        }])
                    })
                }
            };
            info!(flow = %self.flow.name, has_cursor = self.cursor.is_some(), "flow started");
        }

        let src_ctx = self.source_ctx.as_ref().expect("ensured by ensure_built");

        // dry_run path: sampling validation routes through the same
        // tick. `Source::sample` returns a pre-Transform `Batch`;
        // the same Transform program the production tick consumes runs
        // here too so sampling exercises projection / body folding /
        // per-cell conversion identically.
        //
        // Read under source permit only — sink permit is taken later
        // inside `finish_tick → write_and_commit`. Sibling flows that
        // share this source's pool serialise on this acquire; sibling
        // flows that share only the sink/storage are unaffected.
        let raw = {
            let _g = self.flow.lock_handle.acquire_source().await?;
            if dry_run {
                let read_spec = &self.flow.derived().read_spec;
                let n = read_spec.limit;
                let fut = self.flow.source.sample(read_spec, src_ctx, n);
                with_timeout(&self.flow, "sample", fut, &mut self.shutdown).await?
            } else {
                // Production read path: source emits `Batch`; Transform
                // applies projection / body folding / per-column conversion
                // in one pass — including the schemaless-both `["*"]` raw
                // passthrough flow, which lowers to a single `Body` op.
                let read_spec = &self.flow.derived().read_spec;
                let cursor = self.cursor.as_ref();
                let fut = self.flow.source.read_batch(read_spec, src_ctx, cursor);
                with_timeout(&self.flow, "read_batch", fut, &mut self.shutdown).await?
            }
        };

        // Transform is pure compute, no I/O — runs without any permit.
        let batch = self.flow.derived().transform.apply(raw)?;
        self.finish_tick(batch, dry_run).await
    }

    /// Drain phase shared between the dry-run and production paths.
    /// Pulled out so the dry-run early-return can reuse it without
    /// duplicating the empty-batch / write dance. The inter-tick
    /// idle sleep lives in `run` (not here) so it does not extend
    /// the permit-hold region.
    async fn finish_tick(
        &mut self,
        batch: Batch,
        dry_run: bool,
    ) -> Result<TickOutcome, RuntimeError> {
        let batch_size = batch.rows.len();
        if batch_size == 0 {
            if matches!(self.mode, RunMode::Once) {
                debug!(flow = %self.flow.name, "drain complete");
                return Ok(TickOutcome::Exit);
            }
            return Ok(TickOutcome::Idle);
        }
        self.write_and_commit(batch, dry_run).await?;
        if batch_size < self.flow.derived().read_spec.limit && matches!(self.mode, RunMode::Once) {
            return Ok(TickOutcome::Exit);
        }
        Ok(TickOutcome::Continue)
    }

    async fn write_and_commit(&mut self, mut batch: Batch, dry_run: bool) -> RuntimeResult<()> {
        let sink_ctx = self.sink_ctx.as_ref().expect("ensured by ensure_built");
        // Move next_cursor out of the batch — the cursor save below
        // outlives the write call, and the sink does not consume the
        // `next_cursor` slot. `take()` avoids cloning the CursorState.
        let next_cursor = batch.next_cursor.take();
        // No pre-write delete filter here. Sinks whose
        // `supports_deletes() == false` (ClickHouse, QuestDB) drop
        // Delete rows themselves and return a `WriteReport` with the
        // upsert count — the cursor still advances on the call below.
        let write_spec = &self.flow.derived().write_spec;
        // Sink pool I/O under sink permit only — released before we
        // touch storage for the cursor save below.
        let report = {
            let _g = self.flow.lock_handle.acquire_sink().await?;
            let fut = self
                .flow
                .sink
                .write_batch(write_spec, sink_ctx, batch, dry_run);
            match with_timeout(&self.flow, "write_batch", fut, &mut self.shutdown).await {
                Ok(r) => r,
                Err(e) => {
                    error!(flow = %self.flow.name, error = %e, "write_batch failed");
                    return Err(e);
                }
            }
        };

        debug!(
            flow = %self.flow.name,
            rows = report.rows_written,
            "batch written"
        );

        self.commit_cursor(next_cursor, dry_run).await
    }

    async fn commit_cursor(
        &mut self,
        next_cursor: Option<CursorState>,
        dry_run: bool,
    ) -> RuntimeResult<()> {
        if let Some(next) = next_cursor {
            // Storage pool I/O under storage permit only.
            let _g = self.flow.lock_handle.acquire_storage().await?;
            let save_result = match self.flow.cursor_persistence {
                CursorPersistence::ColumnCursor => {
                    let fut = self
                        .flow
                        .storage
                        .save_cursor(&self.flow.name, &next, dry_run);
                    with_timeout(&self.flow, "save_cursor", fut, &mut self.shutdown).await
                }
                CursorPersistence::ResumeToken => {
                    let token_json = extract_resume_token(&next)?;
                    let fut =
                        self.flow
                            .storage
                            .save_resume_token(&self.flow.name, &token_json, dry_run);
                    with_timeout(&self.flow, "save_resume_token", fut, &mut self.shutdown).await
                }
            };
            if let Err(e) = save_result {
                error!(flow = %self.flow.name, error = %e, "cursor save failed; flow will abort to avoid drift");
                return Err(e);
            }
            self.cursor = Some(next);
        } else {
            warn!(
                flow = %self.flow.name,
                "source returned a batch without a next cursor; skipping cursor save"
            );
        }
        Ok(())
    }

    /// Resolve the canonical `DataType` for each `cursor_fields` entry
    /// off the live source schema. Drives the typed
    /// `DataType::decode_cursor_json` dispatch the storage uses to
    /// reload a `CursorState` — without a global registry, the storage
    /// can't know how to reconstruct a `Value::Custom` cursor value
    /// (e.g. ObjectId) by itself.
    ///
    /// Falls back to `DataType::Json` when no schema is available
    /// (schemaless source with empty sample) — cursors over a fully
    /// schemaless source are not officially supported but the path
    /// stays defensive so a misconfigured flow surfaces a clean error
    /// from `decode_cursor_json` rather than a runner panic. Missing
    /// fields surface as `RuntimeError::Validation` so the runner
    /// rebuilds ctx + derived on the next tick.
    fn resolve_cursor_types(&self) -> Result<Vec<crate::types::DataType>, RuntimeError> {
        let cursor_fields = &self.flow.derived().read_spec.cursor_fields;
        if cursor_fields.is_empty() {
            return Ok(Vec::new());
        }
        // Schemaless sources (Mongo) may surface an empty / absent
        // schema when the sampled collection is empty. Fall back to
        // `DataType::Json` so cursor loading keeps working — the
        // storage's `decode_cursor_json` handles the canonical JSON
        // shape regardless of the original Mongo type. For typed
        // sources (`information_schema` is authoritative) a missing
        // field is a real validation error: we still raise
        // `MissingCursorField` so the runner drops + rebuilds ctx.
        let schema = self
            .source_ctx
            .as_ref()
            .and_then(|c| c.as_schema_provider())
            .map(|p| p.schema());
        let src_schemaless = self.flow.source.schemaless();
        let mut out = Vec::with_capacity(cursor_fields.len());
        for field_name in cursor_fields {
            match schema.and_then(|s| s.find(field_name)) {
                Some(f) => out.push(f.data_type.clone()),
                None if src_schemaless => out.push(crate::types::DataType::Json),
                None => {
                    return Err(RuntimeError::Validation(
                        crate::error::ValidationError::MissingCursorField {
                            flow: self.flow.name.clone(),
                            field: field_name.clone(),
                        },
                    ));
                }
            }
        }
        Ok(out)
    }
}

/// Decide whether the runner should drop its cached source/sink ctx
/// Arcs in response to `err`. Backend errors (`Backend`, `Timeout`,
/// `Cancelled`) signal that the underlying connection / driver state
/// may be wedged — refreshing the ctx is the cheapest reliable
/// recovery. Per-row data errors (type / conversion / JSON encoding
/// failures, identifier issues) leave the ctx valid and must NOT
/// trigger a reset. The match is exhaustive so a new variant has to
/// pick a side at compile time.
fn should_drop_ctx_on(err: &RuntimeError) -> bool {
    match err {
        // Backend / connection-level — the underlying driver state may
        // be wedged; refresh ctx so the next iteration rebuilds via
        // `build_context`. Timeout falls in the same bucket: a hung
        // connection won't unwedge by retrying alone.
        RuntimeError::Backend(_) | RuntimeError::Timeout { .. } => true,
        // Shutdown-driven cancellation — not a backend fault. Don't
        // bother rebuilding ctx; the runner is exiting anyway.
        RuntimeError::Cancelled { .. } => false,
        // Per-row data errors — ctx is fine.
        RuntimeError::JsonEncode(_)
        | RuntimeError::Type(_)
        | RuntimeError::Conversion(_)
        | RuntimeError::Serde(_) => false,
        // Validation errors at runtime usually mean schema drift between
        // snapshots — the source schema may have evolved since the last
        // ctx build. A fresh `build_context` re-introspects schemas, so
        // dropping the ctx is the correct response: the next tick will
        // pick up the new shape and rebuild derived plans against it.
        RuntimeError::Validation(_) => true,
        // Programmer / config-level — ctx refresh would just hide it.
        RuntimeError::Io(_)
        | RuntimeError::FlowAborted { .. }
        | RuntimeError::NotRegistered { .. }
        | RuntimeError::Config(_)
        | RuntimeError::ContextMismatch { .. }
        | RuntimeError::SchemaColumnMissing { .. }
        | RuntimeError::Identifier(_)
        | RuntimeError::DerivedPlanInvariant { .. }
        | RuntimeError::Other(_) => false,
    }
}

/// Synthetic cursor field name carrying a serialised resume token.
/// Mirrors `air_elt_source_mongo_cdc::source::RESUME_TOKEN_FIELD`;
/// the runner duplicates the constant to avoid pulling the cdc crate
/// into core. If you change one, change the other.
pub const RESUME_TOKEN_FIELD: &str = "__resume_token";

fn extract_resume_token(state: &CursorState) -> RuntimeResult<serde_json::Value> {
    let field = state
        .fields
        .first()
        .ok_or_else(|| RuntimeError::Other("cdc flow produced an empty cursor state".into()))?;
    if field.name != RESUME_TOKEN_FIELD {
        return Err(RuntimeError::Other(format!(
            "cdc flow produced cursor with unexpected field {:?} (expected {RESUME_TOKEN_FIELD:?})",
            field.name
        )));
    }
    match &field.value {
        crate::types::Value::Json(j) => Ok(j.clone()),
        other => Err(RuntimeError::Other(format!(
            "cdc resume token must be Value::Json, got {other:?}"
        ))),
    }
}

/// Wrap an adapter operation in the flow's `query_timeout` and the
/// runner's shutdown watch. The runtime stays oblivious to driver-level
/// cancel-safety: a driver that cannot tolerate `Drop` mid-await
/// (notably `mongodb` 3.x) is responsible for shielding itself at the
/// call site via `tokio::spawn` so its internal future never gets
/// dropped from here. See `air_elt_commons_mongodb::task::detached`.
async fn with_timeout<F, T>(
    flow: &FlowState,
    op: &'static str,
    fut: F,
    shutdown: &mut watch::Receiver<bool>,
) -> RuntimeResult<T>
where
    F: std::future::Future<Output = RuntimeResult<T>>,
{
    tokio::select! {
        res = tokio::time::timeout(flow.query_timeout, fut) => match res {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(RuntimeError::Timeout {
                flow: flow.name.clone(),
                op,
                after: flow.query_timeout,
            }),
        },
        _ = shutdown.changed() => Err(RuntimeError::Cancelled {
            flow: flow.name.clone(),
            op,
        }),
    }
}

/// Fixed-rate scheduling anchored to UNIX_EPOCH wall clock. The flow's
/// tick grid is the set `{ t : (t - offset_ns) mod interval == 0 }`,
/// where `offset_ns = hash(flow_name) mod interval` (or zero when
/// `jitter` is disabled, collapsing the grid to round wall-clock
/// multiples of `interval`). We sleep to the next future grid point
/// strictly after `now`.
///
/// **Why this design.** The grid is restart-safe (epoch-anchored, not
/// `Instant::now()`-at-start anchored) and skip-to-future by
/// construction — missed ticks are NEVER caught up. If a flow falls
/// behind by 5 intervals (slow query, long backoff, busy semaphore),
/// the next sleep lands on the next future grid point, not at the
/// oldest-missed one. This keeps the per-flow jitter spread stable
/// across the lifetime of the process and across restarts, and
/// prevents back-to-back catchup bursts that would re-cluster flows
/// after any slow phase.
///
/// Pure function — caller samples `SystemTime::now()` and
/// `Instant::now()` once and passes them in; the math is trivially
/// testable without `tokio::time::pause()`.
fn next_tick_instant(
    flow_name: &str,
    interval: Duration,
    jitter: Duration,
    now_since_epoch_ns: u128,
    now: Instant,
) -> Instant {
    let interval_ns = interval.as_nanos().max(1);
    let offset_ns = if jitter.is_zero() {
        0u128
    } else {
        let mut hasher = AHasher::default();
        flow_name.hash(&mut hasher);
        let hash = hasher.finish() as u128;
        // Quantise within `interval` so the offset always falls inside
        // one grid window — `hash mod jitter` may exceed `interval`
        // when `jitter > interval`, which would skew the first grid
        // window by a full period.
        let bound = (jitter.as_nanos()).min(interval_ns);
        if bound == 0 { 0 } else { hash % bound }
    };
    // Compute `(now - offset) mod interval` without ever subtracting
    // into a wrap. `offset_ns < interval_ns` by construction above
    // (`(jitter.as_nanos()).min(interval_ns)`), so adding one
    // interval before subtracting guarantees a non-negative
    // intermediate even if `now_since_epoch_ns < offset_ns` (which
    // never happens in production but would have silently produced a
    // near-2^128 residue under `wrapping_sub`).
    let phase = (now_since_epoch_ns + interval_ns - offset_ns) % interval_ns;
    let time_until_next = interval_ns - phase;
    // `time_until_next` is in [1, interval_ns]; clamp to u64 for the
    // tokio API (interval ceiling is operator-bounded, well under
    // u64::MAX nanoseconds = ~584 years).
    let time_until_next_u64 = u64::try_from(time_until_next).unwrap_or(u64::MAX);
    now + Duration::from_nanos(time_until_next_u64)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::error::JsonEncodeError;
    use crate::flow::test_utils::*;
    use crate::types::Value;

    /// Shared timeline carrier for `shared_source_semaphore_serialises_ticks`.
    /// Defined at module scope so a nested `fn` in the test body can
    /// name it — nested `fn`s do not see types declared inside the
    /// surrounding `async fn`.
    type TimelineHandle = Arc<std::sync::Mutex<Vec<(String, &'static str, std::time::Instant)>>>;

    fn run(flow: FlowState, mode: RunMode, rx: watch::Receiver<bool>) -> FlowRunner {
        FlowRunner::new(flow, mode, rx)
    }

    #[tokio::test(start_paused = true)]
    async fn once_happy_path() {
        let flow = test_flow(mock_source_ok(), mock_sink_ok(), mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        assert!(run(flow, RunMode::Once, rx).run().await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn once_empty_source_completes() {
        let flow = test_flow(mock_source_empty(), mock_sink_ok(), mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        assert!(run(flow, RunMode::Once, rx).run().await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn once_mode_propagates_error() {
        let flow = test_flow(mock_source_failing(1), mock_sink_ok(), mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        let result = run(flow, RunMode::Once, rx).run().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("source boom"));
    }

    #[tokio::test(start_paused = true)]
    async fn save_cursor_failure_aborts_iteration() {
        let flow = test_flow(mock_source_ok(), mock_sink_ok(), mock_storage_save_fails());
        let (_tx, rx) = watch::channel(false);
        let result = run(flow, RunMode::Once, rx).run().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("storage boom"));
    }

    #[tokio::test(start_paused = true)]
    async fn no_cursor_in_batch_skips_save() {
        let flow = test_flow(mock_source_no_cursor(), mock_sink_ok(), mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        assert!(run(flow, RunMode::Once, rx).run().await.is_ok());
    }

    /// `tick(dry_run=true)` routes through `Source::sample`, threads
    /// `dry_run=true` into the sink write, and skips storage cursor
    /// saves (sample produces no `next_cursor`). Asserts read_batch is
    /// NOT called and the sink saw the dry flag.
    #[tokio::test(start_paused = true)]
    async fn tick_dry_run_uses_sample_and_threads_flag() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        let sample_calls = std::sync::Arc::new(AtomicU32::new(0));
        let read_calls = std::sync::Arc::new(AtomicU32::new(0));
        let saw_dry = std::sync::Arc::new(AtomicBool::new(false));

        let mut source = crate::flow::test_utils::default_source_mock();
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            rc.fetch_add(1, Ordering::SeqCst);
            Ok(crate::model::spec::Batch::default())
        });
        let sc = sample_calls.clone();
        source.expect_sample().returning(move |_, _, _| {
            sc.fetch_add(1, Ordering::SeqCst);
            Ok(crate::model::spec::Batch {
                rows: vec![crate::model::spec::Row::upsert(vec![Value::Int64(1)])],
                next_cursor: None,
            })
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(true);
        sink.expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSinkCtx)));
        let sd = saw_dry.clone();
        sink.expect_write_batch()
            .returning(move |_, _ctx, batch, dry| {
                if dry {
                    sd.store(true, Ordering::SeqCst);
                }
                Ok(crate::model::WriteReport {
                    rows_written: batch.rows.len() as u64,
                })
            });

        let mut storage = crate::traits::MockStorage::new();
        storage.expect_load_cursor().returning(|_, _| Ok(None));
        // Sample produces a batch with `next_cursor: None`, so the
        // runner must skip cursor persistence on the dry-run path.
        storage.expect_save_cursor().times(0);

        let flow = test_flow(source, sink, storage);
        let (_tx, rx) = watch::channel(false);
        let mut runner = FlowRunner::new(flow, RunMode::Once, rx);
        runner.tick(true).await.expect("dry-run tick");

        assert_eq!(sample_calls.load(Ordering::SeqCst), 1, "sample must fire");
        assert_eq!(
            read_calls.load(Ordering::SeqCst),
            0,
            "read_batch must NOT fire on dry_run"
        );
        assert!(saw_dry.load(Ordering::SeqCst), "sink must see dry_run=true");
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_backoff_returns_ok() {
        // Build the source inline so we can enforce `.times(1..)`, proving
        // that read_batch was actually called (and failed) before shutdown
        // interrupted the subsequent backoff sleep.
        let mut source = crate::traits::MockSource::new();
        source.expect_schemaless().return_const(false);
        source
            .expect_body_data_type()
            .returning(|| crate::types::DataType::Json);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        source
            .expect_read_batch()
            .times(1..) // must be called at least once; panics on drop if not
            .returning(|_, _, _| Err(crate::error::RuntimeError::Other("source boom".into())));

        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });
        // 500 ms < BACKOFF_INITIAL (1 s): runner is sleeping in backoff when
        // shutdown fires, so the result must be Ok(()).
        tokio::time::advance(Duration::from_millis(500)).await;
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());
    }

    #[test]
    fn should_drop_ctx_picks_backend_variants() {
        use std::time::Duration;
        // Backend / connection-level — drop.
        assert!(should_drop_ctx_on(&RuntimeError::backend(
            std::io::Error::other("e")
        )));
        assert!(should_drop_ctx_on(&RuntimeError::Timeout {
            flow: "f".into(),
            op: "read",
            after: Duration::from_secs(1)
        }));
        // Shutdown cancellation is NOT a backend fault — keep ctx.
        assert!(!should_drop_ctx_on(&RuntimeError::Cancelled {
            flow: "f".into(),
            op: "read"
        }));
        // Per-row data — keep ctx.
        assert!(!should_drop_ctx_on(&RuntimeError::JsonEncode(
            JsonEncodeError::DepthExceeded
        )));
        // Programmer-side — keep ctx.
        assert!(!should_drop_ctx_on(&RuntimeError::Other("x".into())));
        assert!(!should_drop_ctx_on(&RuntimeError::ContextMismatch {
            expected: "T"
        }));
    }

    #[tokio::test(start_paused = true)]
    async fn backend_error_drops_ctx_before_backoff_sleep() {
        use std::sync::atomic::{AtomicU32, Ordering};

        // Read fails with a backend error twice, then succeeds (and the
        // source then drains so RunMode::Daemon eventually idles).
        let read_calls = std::sync::Arc::new(AtomicU32::new(0));
        let build_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::flow::test_utils::default_source_mock();
        source.expect_describe_schema().returning(|_| {
            Ok(crate::model::Schema::new(vec![crate::model::Field {
                name: "id".into(),
                data_type: crate::types::DataType::Int64,
                nullable: false,
            }]))
        });
        let bc = build_calls.clone();
        source.expect_build_context().returning(move |_| {
            bc.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(UnitSourceCtx))
        });
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(RuntimeError::backend(std::io::Error::other("net down")))
            } else {
                // Empty drain → daemon idles.
                Ok(crate::model::spec::Batch::default())
            }
        });

        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });

        // Two backend failures → two backoff cycles. After 1s + 4s sleeps
        // the third tick succeeds; advance in steps so the paused-time
        // scheduler wakes the runner between sleeps.
        for _ in 0..20 {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());

        // Two failures must have rebuilt ctx — initial call + 2 rebuilds = 3.
        // The third successful call reuses the rebuilt ctx, so build is not
        // called again. Allow >= 3 to remain robust to extra ticks while
        // the daemon idles before shutdown.
        let final_build = build_calls.load(Ordering::SeqCst);
        assert!(
            final_build >= 3,
            "expected build_context to fire at least 3 times (initial + 2 rebuilds), got {final_build}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ctx_drop_happens_before_backoff_sleep() {
        // Trigger one backend error and then shut down DURING backoff
        // (before the 1 s sleep elapses). With ordering "drop ctx →
        // sleep", the ctx slot is `None` by the time shutdown fires.
        // Although we cannot inspect the slot directly, we can assert
        // that the ctx-drop side-effect (call to `build_context` on the
        // *next* tick) is not deferred behind the sleep — by ensuring
        // the runner exits cleanly without ever finishing the backoff
        // window. If the drop were AFTER the sleep, shutdown during
        // backoff would skip the drop and the test would still pass —
        // so we additionally assert build_context was called exactly
        // once (the initial tick) and the runner completed cleanly.
        use std::sync::atomic::{AtomicU32, Ordering};

        let build_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::traits::MockSource::new();
        source.expect_schemaless().return_const(false);
        source
            .expect_body_data_type()
            .returning(|| crate::types::DataType::Json);
        let bc = build_calls.clone();
        source.expect_build_context().returning(move |_| {
            bc.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(UnitSourceCtx))
        });
        source
            .expect_read_batch()
            .returning(|_, _, _| Err(RuntimeError::backend(std::io::Error::other("net"))));

        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });

        // 500 ms: well inside the 1 s backoff. Drop must already have
        // happened by now (it runs before the sleep).
        tokio::time::advance(Duration::from_millis(500)).await;
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());

        // build_context called once for the initial tick. The drop
        // arming the next rebuild happened, but the rebuild itself
        // belongs to the next tick which never ran (shutdown during
        // backoff). This is the cleanest signal we can extract without
        // peeking at private state.
        assert_eq!(build_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn json_row_too_large_does_not_drop_ctx() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let build_calls = std::sync::Arc::new(AtomicU32::new(0));
        let read_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::traits::MockSource::new();
        source.expect_schemaless().return_const(false);
        source
            .expect_body_data_type()
            .returning(|| crate::types::DataType::Json);
        let bc = build_calls.clone();
        source.expect_build_context().returning(move |_| {
            bc.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(UnitSourceCtx))
        });
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                Err(RuntimeError::JsonEncode(JsonEncodeError::DepthExceeded))
            } else {
                Ok(crate::model::spec::Batch::default())
            }
        });

        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });

        for _ in 0..20 {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());

        // Per-row JsonEncode errors must NOT trigger ctx drop, so
        // build_context fires exactly once (at the very first tick).
        let final_build = build_calls.load(Ordering::SeqCst);
        let final_reads = read_calls.load(Ordering::SeqCst);
        assert_eq!(
            final_build, 1,
            "per-row JsonEncode must not trigger ctx rebuild (read_calls={final_reads})"
        );
    }

    /// After a backend-error-induced ctx drop, the runner clears
    /// derived AND drops the ctx Arc; the next tick rebuilds the ctx
    /// via `build_context` and re-derives plans. We assert
    /// `build_calls >= 2` (initial + post-error rebuild) — this proves
    /// the ctx was actually rebuilt rather than merely re-using a
    /// pre-built derived snapshot. Use `tokio::time::pause()` so the
    /// backoff sleep doesn't add wall-clock.
    #[tokio::test(start_paused = true)]
    async fn backend_error_clears_derived_and_next_tick_rebuilds() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let read_calls = std::sync::Arc::new(AtomicU32::new(0));
        let build_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::flow::test_utils::default_source_mock();
        let bc = build_calls.clone();
        source.expect_build_context().returning(move |_| {
            bc.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(UnitSourceCtx))
        });
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n < 1 {
                Err(RuntimeError::backend(std::io::Error::other("net down")))
            } else {
                Ok(crate::model::spec::Batch::default())
            }
        });

        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        // Pre-state: derived is always populated (`FlowState::new`
        // requires it). After the backend error the runner drops the
        // ctx Arcs; the next tick must call `build_context` again — that
        // call is what this test verifies.
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });

        for _ in 0..20 {
            tokio::task::yield_now().await;
            tokio::time::advance(Duration::from_secs(1)).await;
        }
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());

        // build_calls >= 2 proves the ctx was rebuilt after the
        // backend error (initial build + post-error rebuild). The
        // pre-built derived path would let this pass at 1, so the
        // tightened bound is what catches a regression where ctx
        // refresh was skipped.
        let final_build = build_calls.load(Ordering::SeqCst);
        assert!(
            final_build >= 2,
            "expected build_context to fire at least 2 times (initial + post-error rebuild), got {final_build}"
        );
    }

    /// Step 5: a direct-only flow (no body, no raw-passthrough) routes
    /// through `Transform::apply` LIVE. Verify by setting up a flow
    /// where the lowered Transform contains a `Convert` (Int16→Int64),
    /// emit an `Int16` row, and assert the sink receives `Int64`. The
    /// legacy `apply_conversions` path would also do this — the test's
    /// purpose is to lock in that the Take-only / Convert{Take} program
    /// is reachable end-to-end through the runner without crashing or
    /// dropping rows. End-to-end value parity with the legacy path was
    /// validated by the Step 4 parallel check (now removed).
    #[tokio::test(start_paused = true)]
    async fn direct_only_flow_runs_through_transform_live() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU32, Ordering};

        let read_calls = std::sync::Arc::new(AtomicU32::new(0));
        let captured: std::sync::Arc<Mutex<Vec<crate::model::Row>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));

        let mut source = crate::flow::test_utils::default_source_mock();
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(crate::model::spec::Batch {
                    rows: vec![crate::model::spec::Row::upsert(vec![Value::Int64(42)])],
                    next_cursor: Some(crate::model::CursorState::new(vec![
                        crate::model::CursorFieldValue {
                            name: "id".into(),
                            value: Value::Int64(42),
                        },
                    ])),
                })
            } else {
                Ok(crate::model::spec::Batch::default())
            }
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(true);
        sink.expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSinkCtx)));
        let cap = captured.clone();
        sink.expect_write_batch()
            .returning(move |_, _ctx, batch, _dry| {
                cap.lock()
                    .unwrap()
                    .extend(batch.rows.iter().map(|r| crate::model::Row {
                        values: r.values.clone(),
                        body: None,
                        op: r.op,
                    }));
                Ok(crate::model::WriteReport {
                    rows_written: batch.rows.len() as u64,
                })
            });

        let flow = test_flow(source, sink, mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        FlowRunner::new(flow, RunMode::Once, rx)
            .run()
            .await
            .expect("flow runs");

        let rows = captured.lock().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].values, vec![Value::Int64(42)]);
    }

    /// Per the post-AIR-70 contract: the runner ships the full batch
    /// (deletes included) to a sink that declares
    /// `supports_deletes() == false`. The sink is responsible for
    /// dropping `RowOp::Delete` rows itself and reporting only the
    /// upsert count back. This test asserts the runner passes the
    /// whole batch through without filtering.
    #[tokio::test(start_paused = true)]
    async fn no_delete_sink_receives_full_batch_runner_does_not_filter() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU32, Ordering};

        let captured: std::sync::Arc<Mutex<Vec<crate::model::Row>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        let read_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::flow::test_utils::default_source_mock();
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(crate::model::spec::Batch {
                    rows: vec![
                        crate::model::spec::Row::upsert(vec![Value::Int64(1)]),
                        crate::model::spec::Row {
                            values: vec![Value::Int64(2)],
                            body: None,
                            op: crate::model::RowOp::Delete,
                        },
                    ],
                    next_cursor: Some(crate::model::CursorState::new(vec![
                        crate::model::CursorFieldValue {
                            name: "id".into(),
                            value: Value::Int64(2),
                        },
                    ])),
                })
            } else {
                Ok(crate::model::spec::Batch::default())
            }
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_schemaless().return_const(false);
        // Append-only sink: it will self-filter inside write_batch.
        sink.expect_supports_deletes().return_const(false);
        sink.expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSinkCtx)));
        let cap = captured.clone();
        sink.expect_write_batch()
            .returning(move |_, _ctx, batch, _dry| {
                // Capture exactly what the runner handed in. Real
                // append-only sinks (ClickHouse, QuestDB) would then
                // filter deletes themselves.
                let upserts = batch
                    .rows
                    .iter()
                    .filter(|r| r.op == crate::model::RowOp::Upsert)
                    .count() as u64;
                cap.lock()
                    .unwrap()
                    .extend(batch.rows.iter().map(|r| crate::model::Row {
                        values: r.values.clone(),
                        body: None,
                        op: r.op,
                    }));
                Ok(crate::model::WriteReport {
                    rows_written: upserts,
                })
            });

        let flow = test_flow(source, sink, mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        FlowRunner::new(flow, RunMode::Once, rx)
            .run()
            .await
            .expect("flow runs");

        let rows = captured.lock().unwrap();
        assert_eq!(
            rows.len(),
            2,
            "sink must observe BOTH rows — runner no longer filters"
        );
        assert_eq!(rows[0].op, crate::model::RowOp::Upsert);
        assert_eq!(rows[1].op, crate::model::RowOp::Delete);
    }

    /// Per the post-AIR-70 contract: an all-delete batch against an
    /// append-only sink still triggers `write_batch` (the sink is the
    /// authoritative filter and reports `rows_written: 0`). The cursor
    /// must still advance so the flow doesn't loop on the same range.
    #[tokio::test(start_paused = true)]
    async fn no_delete_sink_all_deletes_still_calls_write_and_commits_cursor() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU32, Ordering};

        let read_calls = std::sync::Arc::new(AtomicU32::new(0));
        let write_calls = std::sync::Arc::new(AtomicU32::new(0));
        let saved_cursors: std::sync::Arc<Mutex<Vec<crate::model::CursorState>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));

        let mut source = crate::flow::test_utils::default_source_mock();
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(crate::model::spec::Batch {
                    rows: vec![crate::model::spec::Row {
                        values: vec![Value::Int64(7)],
                        body: None,
                        op: crate::model::RowOp::Delete,
                    }],
                    next_cursor: Some(crate::model::CursorState::new(vec![
                        crate::model::CursorFieldValue {
                            name: "id".into(),
                            value: Value::Int64(7),
                        },
                    ])),
                })
            } else {
                Ok(crate::model::spec::Batch::default())
            }
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(false);
        sink.expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSinkCtx)));
        let wc = write_calls.clone();
        sink.expect_write_batch()
            .returning(move |_, _ctx, _batch, _dry| {
                wc.fetch_add(1, Ordering::SeqCst);
                // Sink self-filtered all deletes; nothing to report.
                Ok(crate::model::WriteReport { rows_written: 0 })
            });

        let mut storage = crate::traits::MockStorage::new();
        storage.expect_load_cursor().returning(|_, _| Ok(None));
        let sc = saved_cursors.clone();
        storage
            .expect_save_cursor()
            .returning(move |_, state, _dry| {
                sc.lock().unwrap().push(state.clone());
                Ok(())
            });

        let flow = test_flow(source, sink, storage);
        let (_tx, rx) = watch::channel(false);
        FlowRunner::new(flow, RunMode::Once, rx)
            .run()
            .await
            .expect("flow runs");

        assert_eq!(
            write_calls.load(Ordering::SeqCst),
            1,
            "write_batch must be called once — sink is the authoritative filter"
        );
        let saves = saved_cursors.lock().unwrap();
        assert_eq!(
            saves.len(),
            1,
            "cursor must advance past the delete-only batch"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn daemon_retries_after_failure_then_succeeds() {
        let flow = test_flow(mock_source_failing(2), mock_sink_ok(), mock_storage_ok());
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });
        tokio::time::advance(Duration::from_secs(6)).await;
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());
    }

    /// Per-flow startup jitter must be deterministic — two `FlowRunner`s
    /// constructed from the same name + jitter produce the same offset,
    /// and changing the name produces a different one (almost always —
    /// AHasher distributes evenly enough that a deliberate pair of short
    /// names never collides in practice).
    #[test]
    fn jitter_offset_is_deterministic_per_flow_name() {
        fn build_runner(name: &str, jitter: Duration) -> FlowRunner {
            let mut flow = crate::flow::test_utils::test_flow_named(
                name,
                crate::flow::test_utils::default_source_mock(),
                crate::flow::test_utils::mock_sink_ok(),
                crate::flow::test_utils::mock_storage_ok(),
            );
            // Mutating the assembled half through FlowState's Deref-to-
            // inner doesn't surface; reconstruct via the public ctor.
            let inner = crate::model::AssembledFlow {
                jitter,
                ..(*flow).clone()
            };
            flow = crate::model::FlowState::new(inner, flow.derived().clone());
            let (_tx, rx) = watch::channel(false);
            FlowRunner::new(flow, RunMode::Once, rx)
        }
        let a1 = build_runner("flow-a", Duration::from_secs(1)).jitter_offset();
        let a2 = build_runner("flow-a", Duration::from_secs(1)).jitter_offset();
        let b = build_runner("flow-b", Duration::from_secs(1)).jitter_offset();
        assert_eq!(a1, a2, "same flow name → same offset across constructions");
        assert!(
            a1 < Duration::from_secs(1),
            "offset must lie in [0, jitter)"
        );
        assert_ne!(
            a1, b,
            "distinct flow names must (with overwhelming probability) yield distinct offsets"
        );
    }

    /// Zero `jitter` collapses the offset to `Duration::ZERO` — the
    /// runner must NOT sleep before the first tick.
    #[test]
    fn jitter_offset_zero_when_jitter_disabled() {
        let mut flow = crate::flow::test_utils::test_flow_named(
            "flow-zero",
            crate::flow::test_utils::default_source_mock(),
            crate::flow::test_utils::mock_sink_ok(),
            crate::flow::test_utils::mock_storage_ok(),
        );
        let inner = crate::model::AssembledFlow {
            jitter: Duration::ZERO,
            ..(*flow).clone()
        };
        flow = crate::model::FlowState::new(inner, flow.derived().clone());
        let (_tx, rx) = watch::channel(false);
        let runner = FlowRunner::new(flow, RunMode::Once, rx);
        assert_eq!(runner.jitter_offset(), Duration::ZERO);
    }

    /// With `jitter = 100ms` the runner waits up to 100ms before issuing
    /// the first `read_batch`. Using paused time we advance by the
    /// computed offset and confirm the read fires exactly after that
    /// sleep — never before. The deterministic offset lets us assert
    /// the exact wake-up moment rather than relying on a coarse bound.
    #[tokio::test(start_paused = true)]
    async fn jitter_sleeps_before_first_read() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let read_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::flow::test_utils::default_source_mock();
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(crate::flow::test_utils::UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            rc.fetch_add(1, Ordering::SeqCst);
            Ok(crate::model::spec::Batch::default())
        });

        let mut flow = crate::flow::test_utils::test_flow_named(
            "jitter-flow",
            source,
            crate::flow::test_utils::mock_sink_ok(),
            crate::flow::test_utils::mock_storage_ok(),
        );
        // Replace the test-default `Duration::ZERO` jitter with 100ms so
        // the runner actually sleeps. We rebuild the FlowState via the
        // public constructor; mutating through `Deref<Target=AssembledFlow>`
        // is not surfaced.
        let inner = crate::model::AssembledFlow {
            jitter: Duration::from_millis(100),
            ..(*flow).clone()
        };
        flow = crate::model::FlowState::new(inner, flow.derived().clone());

        let (tx, rx) = watch::channel(false);
        let probe_runner = FlowRunner::new(flow.clone(), RunMode::Daemon, watch::channel(false).1);
        let expected_offset = probe_runner.jitter_offset();
        assert!(expected_offset < Duration::from_millis(100));

        let runner = FlowRunner::new(flow, RunMode::Daemon, rx);
        let handle = tokio::spawn(async move { runner.run().await });

        // Before the offset elapses the runner must still be sleeping —
        // no read should have fired yet.
        if !expected_offset.is_zero() {
            tokio::task::yield_now().await;
            assert_eq!(
                read_calls.load(Ordering::SeqCst),
                0,
                "read_batch must NOT fire before the jitter sleep elapses"
            );
        }
        // Advance past the offset and let the runner reach its first
        // read.
        tokio::time::advance(expected_offset + Duration::from_millis(1)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
            if read_calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::advance(Duration::from_millis(1)).await;
        }
        assert!(
            read_calls.load(Ordering::SeqCst) >= 1,
            "read_batch must fire after the jitter sleep"
        );
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());
    }

    /// Two flows sharing a single-permit source semaphore must execute
    /// their ticks SERIALLY — the second flow cannot enter
    /// `read_batch` until the first releases the permit. Asserted via
    /// per-flow start / end timestamps: the second flow's start must
    /// occur after the first flow's end.
    ///
    /// This is the runtime-side analogue of the validation pipeline's
    /// `FlowLockHandle` test in `util::concurrency`. The semaphore is
    /// the only backpressure primitive at the tick boundary; if it
    /// regressed (e.g. permit released before `write_batch`), the two
    /// flows would interleave and the assertion would fail.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_source_semaphore_serialises_ticks() {
        use std::sync::Mutex;

        // Build ONE manager with a single-permit source "shared-src"
        // and effectively-unbounded sinks/storages. Both flows then
        // ask the same manager for a handle pointing at "shared-src",
        // so they wind up with the same `Arc<Semaphore>` for the
        // source slot and naturally serialise on it.
        let mut mgr = crate::util::ConcurrencyManager::new();
        mgr.register_source("shared-src", 1);
        mgr.register_sink("sink-1", u32::MAX);
        mgr.register_sink("sink-2", u32::MAX);
        mgr.register_storage("storage-1", u32::MAX);
        mgr.register_storage("storage-2", u32::MAX);

        // Timeline recorded by each flow's mocked read_batch — we
        // capture (flow_name, "start" | "end", Instant::now()).
        let timeline: TimelineHandle = Arc::new(Mutex::new(Vec::new()));

        fn build_flow(
            flow_name: &str,
            sink_name: &str,
            storage_name: &str,
            mgr: &crate::util::ConcurrencyManager,
            timeline: TimelineHandle,
        ) -> FlowState {
            let mut source = crate::flow::test_utils::default_source_mock();
            source
                .expect_build_context()
                .returning(|_| Ok(Arc::new(crate::flow::test_utils::UnitSourceCtx)));
            let name_owned = flow_name.to_string();
            let tl_for_read = timeline.clone();
            let read_counter = Arc::new(std::sync::atomic::AtomicU32::new(0));
            source.expect_read_batch().returning(move |_, _, _| {
                let n = read_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    tl_for_read.lock().unwrap().push((
                        name_owned.clone(),
                        "start",
                        std::time::Instant::now(),
                    ));
                    // Hold the permit-protected region open for a short
                    // window so concurrent interleaving (had it been
                    // allowed) would have been observable.
                    std::thread::sleep(Duration::from_millis(50));
                    tl_for_read.lock().unwrap().push((
                        name_owned.clone(),
                        "end",
                        std::time::Instant::now(),
                    ));
                    Ok(crate::model::spec::Batch {
                        rows: vec![crate::model::spec::Row::upsert(vec![Value::Int64(1)])],
                        next_cursor: Some(crate::model::CursorState::new(vec![
                            crate::model::CursorFieldValue {
                                name: "id".into(),
                                value: Value::Int64(1),
                            },
                        ])),
                    })
                } else {
                    Ok(crate::model::spec::Batch::default())
                }
            });

            let state = crate::flow::test_utils::test_flow_named(
                flow_name,
                source,
                crate::flow::test_utils::mock_sink_ok(),
                crate::flow::test_utils::mock_storage_ok(),
            );
            // Replace the auto-built (unbounded) handle with one that
            // points at the shared "shared-src" semaphore — this is
            // the wiring the validation pipeline does at assemble
            // time for two flows pointing at the same `[[sources]]`
            // entry.
            let inner = crate::model::AssembledFlow {
                lock_handle: mgr.handle("shared-src", sink_name, storage_name),
                ..(*state).clone()
            };
            crate::model::FlowState::new(inner, state.derived().clone())
        }

        let f1 = build_flow("flow-1", "sink-1", "storage-1", &mgr, timeline.clone());
        let f2 = build_flow("flow-2", "sink-2", "storage-2", &mgr, timeline.clone());

        let (_tx, rx1) = watch::channel(false);
        let (_tx2, rx2) = watch::channel(false);
        let h1 = tokio::spawn(async move { FlowRunner::new(f1, RunMode::Once, rx1).run().await });
        let h2 = tokio::spawn(async move { FlowRunner::new(f2, RunMode::Once, rx2).run().await });

        h1.await.unwrap().expect("flow-1 ok");
        h2.await.unwrap().expect("flow-2 ok");

        // The two flows must NOT overlap. Find each flow's read-region
        // and assert the later one starts after the earlier one ends.
        let tl = timeline.lock().unwrap();
        let f1_start = tl
            .iter()
            .find(|(n, k, _)| n == "flow-1" && *k == "start")
            .expect("flow-1 start");
        let f1_end = tl
            .iter()
            .find(|(n, k, _)| n == "flow-1" && *k == "end")
            .expect("flow-1 end");
        let f2_start = tl
            .iter()
            .find(|(n, k, _)| n == "flow-2" && *k == "start")
            .expect("flow-2 start");
        let f2_end = tl
            .iter()
            .find(|(n, k, _)| n == "flow-2" && *k == "end")
            .expect("flow-2 end");

        let serialised = f2_start.2 >= f1_end.2 || f1_start.2 >= f2_end.2;
        assert!(
            serialised,
            "flows interleaved despite single-permit semaphore: \
             f1=({:?}..{:?}), f2=({:?}..{:?})",
            f1_start.2, f1_end.2, f2_start.2, f2_end.2
        );
    }

    /// `next_tick_instant` returns a strictly future Instant landing on
    /// the canonical grid (`(t - offset) mod interval == 0`). With
    /// `jitter = 0s` offset is zero, so the grid is round multiples of
    /// `interval`.
    #[test]
    fn next_tick_instant_zero_jitter_lands_on_round_multiples() {
        let now = Instant::now();
        let interval = Duration::from_secs(1);
        // 12.5s past epoch → next round-second is 13.0s, 500ms away.
        let now_since_epoch_ns: u128 = 12_500_000_000;
        let next = next_tick_instant("any", interval, Duration::ZERO, now_since_epoch_ns, now);
        assert_eq!(next - now, Duration::from_millis(500));
    }

    /// Two flows with the same interval but different names must land
    /// on different grid points (offset distributed by the hasher).
    #[test]
    fn next_tick_instant_offsets_differ_per_flow_name() {
        let now = Instant::now();
        let interval = Duration::from_secs(1);
        let jitter = Duration::from_secs(1);
        let now_ns: u128 = 12_500_000_000;
        let a = next_tick_instant("flow-a", interval, jitter, now_ns, now);
        let b = next_tick_instant("flow-b", interval, jitter, now_ns, now);
        // Both must land inside `(now, now + interval]`.
        assert!(a > now && a <= now + interval);
        assert!(b > now && b <= now + interval);
        // With overwhelming probability AHasher distributes two short
        // names to different residues mod 1s.
        assert_ne!(a, b);
    }

    /// Skip-to-future semantics: simulate the current wall-clock time
    /// being 5×interval past the previous grid point. The next tick
    /// must land at the FIRST future grid point — never queue up four
    /// missed ticks for back-to-back replay.
    #[test]
    fn next_tick_instant_skips_missed_grid_points() {
        let now = Instant::now();
        let interval = Duration::from_secs(1);
        // 5.2s past epoch, zero jitter → next grid point is at 6.0s,
        // 800ms away. Crucially NOT 1s-1s-1s-1s-200ms (back-to-back
        // catchup) — just one future point.
        let now_since_epoch_ns: u128 = 5_200_000_000;
        let next = next_tick_instant("any", interval, Duration::ZERO, now_since_epoch_ns, now);
        assert_eq!(next - now, Duration::from_millis(800));
        // Sanity: the gap is strictly less than `interval`.
        assert!(next - now < interval);
    }

    /// Exact grid-point boundary: when `(t - offset) mod interval == 0`,
    /// the next tick must land one full interval ahead (not now itself).
    /// Otherwise `sleep_until(now)` would return immediately and the
    /// next iteration would re-tick instantly — a tight spin loop.
    #[test]
    fn next_tick_instant_on_boundary_jumps_one_interval() {
        let now = Instant::now();
        let interval = Duration::from_secs(1);
        let now_since_epoch_ns: u128 = 7_000_000_000;
        let next = next_tick_instant("any", interval, Duration::ZERO, now_since_epoch_ns, now);
        assert_eq!(next - now, interval);
    }
}
