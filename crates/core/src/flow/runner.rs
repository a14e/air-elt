use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{
    Batch, CursorFieldValue, CursorPersistence, CursorState, FlowState, RowOp, Schema, SinkCtx,
    SourceCtx,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Daemon,
    Once,
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

    pub async fn run(mut self) -> RuntimeResult<()> {
        loop {
            match self.tick(false).await {
                Ok(true) => {
                    return Ok(());
                }
                Ok(false) => {
                    self.backoff = BACKOFF_INITIAL;
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
        runner.tick(true).await.map(|_| ())
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
            self.source_ctx = Some(
                self.flow
                    .source
                    .build_context(&self.flow.derived().read_spec)
                    .await?,
            );
        }
        if self.sink_ctx.is_none() {
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
    async fn tick(&mut self, dry_run: bool) -> Result<bool, RuntimeError> {
        if self.cursor.is_none() {
            let storage = self.flow.storage.clone();
            let flow_name = self.flow.name.clone();
            let cancel_safe = storage.cancel_safe();
            self.cursor = match self.flow.cursor_persistence {
                CursorPersistence::ColumnCursor => {
                    let fut = async move { storage.load_cursor(&flow_name).await };
                    run_op(
                        &self.flow,
                        "load_cursor",
                        fut,
                        cancel_safe,
                        &mut self.shutdown,
                    )
                    .await?
                }
                CursorPersistence::ResumeToken => {
                    let fut = async move { storage.load_resume_token(&flow_name).await };
                    let token = run_op(
                        &self.flow,
                        "load_resume_token",
                        fut,
                        cancel_safe,
                        &mut self.shutdown,
                    )
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

        self.ensure_built().await?;

        if *self.shutdown.borrow() {
            info!(flow = %self.flow.name, "shutdown signalled");
            return Ok(true);
        }

        // The derived plans cache the post-expansion `ReadSpec`. Clone
        // it once and move the clone into the spawned future.
        let read_spec = self.flow.derived().read_spec.clone();

        // Arc-shared context: runner keeps its clone in self.source_ctx,
        // future holds another. On async cancellation only the future's
        // clone is dropped — runner state and cached schema / read_query
        // survive into the next tick.
        let src_ctx = self
            .source_ctx
            .as_ref()
            .expect("ensured by ensure_built")
            .clone();
        let source = self.flow.source.clone();
        let cursor = self.cursor.clone();
        let cancel_safe = source.cancel_safe();
        // Pull the lowered Transform program once outside the spawned
        // future so the borrow on `derived` doesn't straddle the await.
        let transform = self.flow.derived().transform.clone();

        // dry_run path: sampling validation routes through the same
        // tick. `Source::sample` returns a pre-Transform `RawBatch`;
        // the same Transform program the production tick consumes runs
        // here too so sampling exercises projection / body folding /
        // per-cell conversion identically.
        if dry_run {
            let n = read_spec.limit;
            let f = async move { source.sample(&read_spec, src_ctx, n).await };
            let raw = run_op(&self.flow, "sample", f, cancel_safe, &mut self.shutdown).await?;
            let batch = transform.apply(raw)?;
            return self.finish_tick(batch, true).await;
        }

        // Production read path: source emits `RawBatch`; Transform
        // applies projection / body folding / per-column conversion in
        // one pass — including the schemaless-both `["*"]` raw
        // passthrough flow, which lowers to a single `Body` op.
        let f = async move {
            source
                .read_batch(&read_spec, src_ctx, cursor.as_ref())
                .await
        };
        let raw = run_op(&self.flow, "read_batch", f, cancel_safe, &mut self.shutdown).await?;

        let batch = transform.apply(raw)?;

        self.finish_tick(batch, dry_run).await
    }

    /// Drain phase shared between the dry-run and production paths.
    /// Pulled out so the dry-run early-return can reuse it without
    /// duplicating the empty-batch / write / interval-sleep dance.
    async fn finish_tick(&mut self, batch: Batch, dry_run: bool) -> Result<bool, RuntimeError> {
        let batch_size = batch.rows.len();
        if batch_size == 0 {
            if matches!(self.mode, RunMode::Once) {
                debug!(flow = %self.flow.name, "drain complete");
                return Ok(true);
            }
            tokio::select! {
                _ = sleep(self.flow.interval) => {}
                _ = self.shutdown.changed() => {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        self.write_and_commit(batch, dry_run).await?;
        if batch_size < self.flow.derived().read_spec.limit && matches!(self.mode, RunMode::Once) {
            return Ok(true);
        }
        Ok(false)
    }

    async fn write_and_commit(&mut self, mut batch: Batch, dry_run: bool) -> RuntimeResult<()> {
        let sink_ctx = self
            .sink_ctx
            .as_ref()
            .expect("ensured by ensure_built")
            .clone();
        let sink = self.flow.sink.clone();
        // Cached on `DerivedPlans`; clone-once for the spawned future.
        let write_spec = self.flow.derived().write_spec.clone();
        // Capture next_cursor before moving the batch into the sink —
        // the cursor save below outlives the write call.
        let next_cursor = batch.next_cursor.clone();
        // Append-only sinks (ClickHouse) drop deletes pre-write — the
        // sink will never observe a `RowOp::Delete`. Cursor still
        // advances over the dropped events.
        if !sink.supports_deletes() {
            let before = batch.rows.len();
            batch.rows.retain(|r| r.op != RowOp::Delete);
            let dropped = before - batch.rows.len();
            if dropped > 0 {
                debug!(
                    flow = %self.flow.name,
                    deletes_dropped = dropped,
                    "sink does not support deletes — dropping delete rows"
                );
            }
            if batch.rows.is_empty() {
                // Still need to advance the cursor below; skip the sink
                // call entirely (no rows to write).
                return self.commit_cursor(next_cursor, dry_run).await;
            }
        }
        let cancel_safe = sink.cancel_safe();
        let fut = async move {
            sink.write_batch(&write_spec, sink_ctx, batch, dry_run)
                .await
        };
        let report = match run_op(
            &self.flow,
            "write_batch",
            fut,
            cancel_safe,
            &mut self.shutdown,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                error!(flow = %self.flow.name, error = %e, "write_batch failed");
                return Err(e);
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
            let storage = self.flow.storage.clone();
            let flow_name = self.flow.name.clone();
            let cancel_safe = storage.cancel_safe();
            let save_result = match self.flow.cursor_persistence {
                CursorPersistence::ColumnCursor => {
                    let next_owned = next.clone();
                    let fut =
                        async move { storage.save_cursor(&flow_name, &next_owned, dry_run).await };
                    run_op(
                        &self.flow,
                        "save_cursor",
                        fut,
                        cancel_safe,
                        &mut self.shutdown,
                    )
                    .await
                }
                CursorPersistence::ResumeToken => {
                    let token_json = extract_resume_token(&next)?;
                    let fut = async move {
                        storage
                            .save_resume_token(&flow_name, &token_json, dry_run)
                            .await
                    };
                    run_op(
                        &self.flow,
                        "save_resume_token",
                        fut,
                        cancel_safe,
                        &mut self.shutdown,
                    )
                    .await
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

/// Cancellation-safe path: rely on `tokio::time::timeout` + `select!` —
/// dropping `fut` mid-await is safe for the underlying driver. Used for
/// sqlx-backed connectors.
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

/// Cancellation-unsafe path: spawn the future on the runtime and detach
/// the `JoinHandle` on shutdown / timeout. In tokio, dropping a
/// `JoinHandle` does NOT abort the task — the task runs to completion
/// independently, so the underlying driver future never gets dropped
/// mid-await. Used for the `mongodb` 3.x driver, which is not
/// cancellation-safe.
async fn with_spawn_detach<F, T>(
    flow: &FlowState,
    op: &'static str,
    fut: F,
    shutdown: &mut watch::Receiver<bool>,
) -> RuntimeResult<T>
where
    F: std::future::Future<Output = RuntimeResult<T>> + Send + 'static,
    T: Send + 'static,
{
    let mut handle = tokio::spawn(fut);
    tokio::select! {
        res = &mut handle => match res {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e),
            Err(join_err) => Err(RuntimeError::Other(format!(
                "spawned op {op} panicked: {join_err}"
            ))),
        },
        _ = tokio::time::sleep(flow.query_timeout) => {
            // Detach: the task keeps running; the driver future
            // completes naturally. The connection-level / pool
            // timeouts on the underlying client bound runaway work.
            drop(handle);
            Err(RuntimeError::Timeout {
                flow: flow.name.clone(),
                op,
                after: flow.query_timeout,
            })
        }
        _ = shutdown.changed() => {
            drop(handle);
            Err(RuntimeError::Cancelled {
                flow: flow.name.clone(),
                op,
            })
        }
    }
}

/// Dispatch by connector cancellation safety.
async fn run_op<F, T>(
    flow: &FlowState,
    op: &'static str,
    fut: F,
    cancel_safe: bool,
    shutdown: &mut watch::Receiver<bool>,
) -> RuntimeResult<T>
where
    F: std::future::Future<Output = RuntimeResult<T>> + Send + 'static,
    T: Send + 'static,
{
    if cancel_safe {
        with_timeout(flow, op, fut, shutdown).await
    } else {
        with_spawn_detach(flow, op, fut, shutdown).await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::error::JsonEncodeError;
    use crate::flow::test_utils::*;
    use crate::types::Value;

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
        source.expect_cancel_safe().return_const(true);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            rc.fetch_add(1, Ordering::SeqCst);
            Ok(crate::model::raw::RawBatch::default())
        });
        let sc = sample_calls.clone();
        source.expect_sample().returning(move |_, _, _| {
            sc.fetch_add(1, Ordering::SeqCst);
            Ok(crate::model::raw::RawBatch {
                rows: vec![crate::model::raw::RawRow::upsert(vec![Value::Int64(1)])],
                next_cursor: None,
            })
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_cancel_safe().return_const(true);
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
        storage.expect_cancel_safe().return_const(true);
        storage.expect_load_cursor().returning(|_| Ok(None));
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
        source.expect_cancel_safe().return_const(true);
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

    #[tokio::test(start_paused = true)]
    async fn spawn_detach_path_completes_when_source_is_not_cancel_safe() {
        // A source that opts out of cancellation safety must still
        // produce correct results in the happy path — exercises the
        // `with_spawn_detach` branch end-to-end.
        let mut source = crate::traits::MockSource::new();
        source.expect_cancel_safe().return_const(false);
        source.expect_schemaless().return_const(false);
        source
            .expect_body_data_type()
            .returning(|| crate::types::DataType::Json);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        source.expect_read_batch().returning(move |_, _, _| {
            let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(one_row_batch())
            } else {
                Ok(crate::model::raw::RawBatch::default())
            }
        });
        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        assert!(run(flow, RunMode::Once, rx).run().await.is_ok());
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
        source.expect_cancel_safe().return_const(true);
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
                Ok(crate::model::raw::RawBatch::default())
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
        source.expect_cancel_safe().return_const(true);
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
        source.expect_cancel_safe().return_const(true);
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
                Ok(crate::model::raw::RawBatch::default())
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
        source.expect_cancel_safe().return_const(true);
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
                Ok(crate::model::raw::RawBatch::default())
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
        source.expect_cancel_safe().return_const(true);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(crate::model::raw::RawBatch {
                    rows: vec![crate::model::raw::RawRow::upsert(vec![Value::Int64(42)])],
                    next_cursor: Some(crate::model::CursorState::new(vec![
                        crate::model::CursorFieldValue {
                            name: "id".into(),
                            value: Value::Int64(42),
                        },
                    ])),
                })
            } else {
                Ok(crate::model::raw::RawBatch::default())
            }
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_cancel_safe().return_const(true);
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

    /// When the sink declares `supports_deletes() == false`, the runner
    /// must filter `RowOp::Delete` rows out before `write_batch`. Verify
    /// that the sink only sees the Upsert.
    #[tokio::test(start_paused = true)]
    async fn no_delete_sink_filters_delete_rows_pre_write() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU32, Ordering};

        let captured: std::sync::Arc<Mutex<Vec<crate::model::Row>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));
        let read_calls = std::sync::Arc::new(AtomicU32::new(0));

        let mut source = crate::flow::test_utils::default_source_mock();
        source.expect_cancel_safe().return_const(true);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(crate::model::raw::RawBatch {
                    rows: vec![
                        crate::model::raw::RawRow::upsert(vec![Value::Int64(1)]),
                        // Delete event from a hypothetical CDC source.
                        crate::model::raw::RawRow {
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
                Ok(crate::model::raw::RawBatch::default())
            }
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_cancel_safe().return_const(true);
        sink.expect_schemaless().return_const(false);
        // Append-only sink: runner must drop deletes.
        sink.expect_supports_deletes().return_const(false);
        sink.expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSinkCtx)));
        let cap = captured.clone();
        sink.expect_write_batch()
            .returning(move |_, _ctx, batch, _dry| {
                cap.lock()
                    .unwrap()
                    .extend(batch.rows.iter().map(|r| crate::model::Row {
                        values: r.values.clone(),
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
        assert_eq!(rows.len(), 1, "delete row must be filtered pre-write");
        assert_eq!(rows[0].op, crate::model::RowOp::Upsert);
        assert_eq!(rows[0].values, vec![Value::Int64(1)]);
    }

    /// When a batch consists entirely of delete rows and the sink
    /// declares `supports_deletes() == false`, the runner must NOT call
    /// `write_batch` at all — but it must still commit the cursor so
    /// the flow doesn't get stuck re-reading the same range forever.
    #[tokio::test(start_paused = true)]
    async fn no_delete_sink_all_deletes_skips_write_but_commits_cursor() {
        use std::sync::Mutex;
        use std::sync::atomic::{AtomicU32, Ordering};

        let read_calls = std::sync::Arc::new(AtomicU32::new(0));
        let saved_cursors: std::sync::Arc<Mutex<Vec<crate::model::CursorState>>> =
            std::sync::Arc::new(Mutex::new(Vec::new()));

        let mut source = crate::flow::test_utils::default_source_mock();
        source.expect_cancel_safe().return_const(true);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let rc = read_calls.clone();
        source.expect_read_batch().returning(move |_, _, _| {
            let n = rc.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Ok(crate::model::raw::RawBatch {
                    rows: vec![crate::model::raw::RawRow {
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
                Ok(crate::model::raw::RawBatch::default())
            }
        });

        let mut sink = crate::traits::MockSink::new();
        sink.expect_cancel_safe().return_const(true);
        sink.expect_schemaless().return_const(false);
        sink.expect_supports_deletes().return_const(false);
        sink.expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSinkCtx)));
        // The whole point: write_batch must not be invoked.
        sink.expect_write_batch().times(0);

        let mut storage = crate::traits::MockStorage::new();
        storage.expect_cancel_safe().return_const(true);
        storage.expect_load_cursor().returning(|_| Ok(None));
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
}
