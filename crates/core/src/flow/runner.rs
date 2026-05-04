use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{
    Batch, CursorFieldValue, CursorPersistence, CursorState, FlowState, SinkCtx, SourceCtx,
};
use crate::types::{Value, convert};

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
            match self.tick().await {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    self.backoff = BACKOFF_INITIAL;
                }
                Err(e) => {
                    if matches!(self.mode, RunMode::Once) {
                        return Err(e);
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

    async fn ensure_contexts(&mut self) -> RuntimeResult<()> {
        if self.source_ctx.is_none() {
            self.source_ctx = Some(self.flow.source.build_context(&self.flow.read_spec).await?);
        }
        if self.sink_ctx.is_none() {
            self.sink_ctx = Some(self.flow.sink.build_context(&self.flow.write_spec).await?);
        }
        Ok(())
    }

    async fn tick(&mut self) -> Result<bool, RuntimeError> {
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

        self.ensure_contexts().await?;

        if *self.shutdown.borrow() {
            info!(flow = %self.flow.name, "shutdown signalled");
            return Ok(true);
        }

        // Arc-shared context: runner keeps its clone in self.source_ctx,
        // future holds another. On async cancellation only the future's
        // clone is dropped — runner state and cached schema / read_query
        // survive into the next tick.
        let src_ctx = self
            .source_ctx
            .as_ref()
            .expect("ensured by ensure_contexts")
            .clone();
        let source = self.flow.source.clone();
        let read_spec = self.flow.read_spec.clone();
        let cursor = self.cursor.clone();
        let cancel_safe = source.cancel_safe();
        let fut = async move {
            source
                .read_batch(&read_spec, src_ctx, cursor.as_ref())
                .await
        };
        let batch = run_op(
            &self.flow,
            "read_batch",
            fut,
            cancel_safe,
            &mut self.shutdown,
        )
        .await?;

        let batch = apply_conversions(batch, &self.flow.conversions)?;
        let batch = dedup_cdc_batch(batch, &self.flow);
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

        self.write_and_commit(&batch).await?;

        if batch_size < self.flow.read_spec.limit && matches!(self.mode, RunMode::Once) {
            return Ok(true);
        }
        Ok(false)
    }

    async fn write_and_commit(&mut self, batch: &Batch) -> RuntimeResult<()> {
        let sink_ctx = self
            .sink_ctx
            .as_ref()
            .expect("ensured by ensure_contexts")
            .clone();
        let sink = self.flow.sink.clone();
        let write_spec = self.flow.write_spec.clone();
        let owned_batch = batch.clone();
        let cancel_safe = sink.cancel_safe();
        let fut = async move { sink.write_batch(&write_spec, sink_ctx, &owned_batch).await };
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

        if let Some(next) = &batch.next_cursor {
            let storage = self.flow.storage.clone();
            let flow_name = self.flow.name.clone();
            let cancel_safe = storage.cancel_safe();
            let save_result = match self.flow.cursor_persistence {
                CursorPersistence::ColumnCursor => {
                    let next_owned = next.clone();
                    let fut = async move { storage.save_cursor(&flow_name, &next_owned).await };
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
                    let token_json = extract_resume_token(next)?;
                    let fut =
                        async move { storage.save_resume_token(&flow_name, &token_json).await };
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
            self.cursor = Some(next.clone());
        } else {
            warn!(
                flow = %self.flow.name,
                "source returned a batch without a next cursor; skipping cursor save"
            );
        }
        Ok(())
    }
}

/// Walk every cell whose source DataType differs from its sink DataType and
/// dispatch through `types::convert::convert`. Identity columns are
/// untouched. Empty `conversions` (e.g. tests that skip validation) and
/// all-identity column lists short-circuit — no per-row allocation happens
/// in either case.
/// CDC compaction: a single change-stream batch may carry several
/// events for the same document key. The dangerous case is
/// `delete(k) → insert(k)`: our sink applies upserts before deletes
/// to keep `insert(k) → delete(k)` ordering correct, which inverts
/// the intent here. Compact by keeping only the latest event per
/// `conflict.key` tuple, walking in reverse so the survivor is the
/// chronologically-last event for that key.
///
/// Hot-path shortcuts:
/// * non-CDC flows skip entirely (`ColumnCursor` never produces mixed-op batches);
/// * batches without a single `Delete` skip — duplicate Upserts on
///   the same key are idempotent (overwrite = same result; ignore =
///   "first wins" is acceptable);
/// * single-row batches skip.
///
/// When dedup does run: walk `batch.rows.into_iter().rev()`, build a
/// per-row fingerprint via `Row::raw_key` (per-`Value`-variant byte
/// encoder, see `core::types::raw_key`) into a reused buffer, and
/// insert into an `AHashSet<Vec<u8>>`. First-seen wins per key; row
/// is pushed into `kept` and `kept.reverse()`s once at the end so
/// survivors retain their relative arrival order (matters for
/// upsert-then-delete sink semantics on distinct keys). The key
/// indices come from `FlowState::dedup_key_indices` — pre-computed
/// once at assemble-time, not per row.
fn dedup_cdc_batch(batch: Batch, flow: &FlowState) -> Batch {
    if flow.cursor_persistence != CursorPersistence::ResumeToken || batch.rows.len() <= 1 {
        return batch;
    }
    if !batch
        .rows
        .iter()
        .any(|r| r.op == crate::model::RowOp::Delete)
    {
        return batch;
    }
    let Some(key_indices) = flow.dedup_key_indices() else {
        return batch;
    };

    // Walk from the end, keep first-seen-key only. `seen` holds raw
    // fingerprint bytes produced by `Row::raw_key` (per-variant byte
    // encoder, see `core::types::raw_key`). Reverse iteration makes
    // the survivor the chronologically-last event per key, matching
    // CDC compaction. Each iteration allocates a fresh `buf` and
    // moves it into `seen` directly — no clone-copy step. On a
    // duplicate the move drops the buf; the alloc cost is the same
    // either way (`HashSet::insert` always owns the key it stores).
    let n = batch.rows.len();
    let mut seen: ahash::AHashSet<Vec<u8>> = ahash::AHashSet::with_capacity(n);
    let mut kept: Vec<crate::model::Row> = Vec::with_capacity(n);
    for row in batch.rows.into_iter().rev() {
        let mut buf: Vec<u8> = Vec::with_capacity(32);
        row.raw_key(key_indices, &mut buf);
        if seen.insert(buf) {
            kept.push(row);
        }
    }
    kept.reverse();
    Batch {
        rows: kept,
        next_cursor: batch.next_cursor,
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

fn apply_conversions(
    mut batch: Batch,
    conversions: &[crate::model::ConversionPlan],
) -> RuntimeResult<Batch> {
    if conversions.is_empty() || conversions.iter().all(|p| p.is_identity()) {
        return Ok(batch);
    }
    for row in &mut batch.rows {
        if row.values.len() != conversions.len() {
            return Err(RuntimeError::Other(format!(
                "row has {} values but {} conversions configured — schema drift?",
                row.values.len(),
                conversions.len()
            )));
        }
        for (slot, plan) in row.values.iter_mut().zip(conversions.iter()) {
            if plan.is_identity() {
                continue;
            }
            let owned = std::mem::replace(slot, Value::Null);
            *slot = convert::convert(owned, &plan.source, &plan.sink, &plan.ctx)?;
        }
    }
    Ok(batch)
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
    use crate::flow::test_utils::*;

    fn run(flow: FlowState, mode: RunMode, rx: watch::Receiver<bool>) -> FlowRunner {
        FlowRunner::new(flow, mode, rx)
    }

    mod dedup {
        use std::sync::Arc;
        use std::time::Duration;

        use super::super::dedup_cdc_batch;
        use crate::config::conflict::{ConflictConfig, ConflictStrategy};
        use crate::config::model::CursorOrder;
        use crate::config::validation::SamplingConfig;
        use crate::model::{
            AssembledFlow, Batch, CursorPersistence, FlowState, ReadSpec, Row, RowOp, WriteSpec,
        };
        use crate::traits::{MockSink, MockSource, MockStorage};
        use crate::types::Value;

        fn flow_with(persistence: CursorPersistence, conflict_key: Option<Vec<&str>>) -> FlowState {
            let conflict = conflict_key.map(|k| ConflictConfig {
                key: k.into_iter().map(String::from).collect(),
                strategy: ConflictStrategy::Overwrite,
            });
            let assembled = AssembledFlow {
                name: "dedup_test".into(),
                source: Arc::new(MockSource::new()),
                sink: Arc::new(MockSink::new()),
                storage: Arc::new(MockStorage::new()),
                mappings: Vec::new(),
                read_spec: ReadSpec {
                    columns: vec!["id".into(), "name".into()],
                    table: "t".into(),
                    cursor_fields: Vec::new(),
                    cursor_order: CursorOrder::Asc,
                    limit: 1,
                    source_options: toml::Table::new(),
                },
                write_spec: WriteSpec {
                    columns: vec!["id".into(), "name".into()],
                    table: "t".into(),
                    conflict,
                },
                interval: Duration::from_millis(10),
                query_timeout: Duration::from_secs(5),
                sampling: SamplingConfig::Disabled,
                access_check: false,
                fields_check: false,
                inserts_check: false,
                cursor_persistence: persistence,
            };
            FlowState::new_unchecked(assembled, Vec::new())
        }

        fn cdc_flow_with_conflict(key: Vec<&str>) -> FlowState {
            flow_with(CursorPersistence::ResumeToken, Some(key))
        }

        fn row(op: RowOp, id: i64, name: &str) -> Row {
            Row {
                op,
                values: vec![Value::Int64(id), Value::Text(name.into())],
            }
        }

        #[test]
        fn keeps_only_last_op_per_key() {
            // delete(1) → insert(1) — the insert is the survivor.
            // Without dedup, the sink would upsert(1) then delete(1)
            // and the row would disappear.
            let flow = cdc_flow_with_conflict(vec!["id"]);
            let batch = Batch {
                rows: vec![row(RowOp::Delete, 1, ""), row(RowOp::Upsert, 1, "alice")],
                next_cursor: None,
            };
            let out = dedup_cdc_batch(batch, &flow);
            assert_eq!(out.rows.len(), 1);
            assert_eq!(out.rows[0].op, RowOp::Upsert);
            assert_eq!(out.rows[0].values[1], Value::Text("alice".into()));
        }

        #[test]
        fn delete_after_insert_survives_as_delete() {
            let flow = cdc_flow_with_conflict(vec!["id"]);
            let batch = Batch {
                rows: vec![row(RowOp::Upsert, 1, "alice"), row(RowOp::Delete, 1, "")],
                next_cursor: None,
            };
            let out = dedup_cdc_batch(batch, &flow);
            assert_eq!(out.rows.len(), 1);
            assert_eq!(out.rows[0].op, RowOp::Delete);
        }

        #[test]
        fn distinct_keys_kept_in_order() {
            let flow = cdc_flow_with_conflict(vec!["id"]);
            let batch = Batch {
                rows: vec![
                    row(RowOp::Upsert, 1, "a"),
                    row(RowOp::Upsert, 2, "b"),
                    row(RowOp::Delete, 3, ""),
                ],
                next_cursor: None,
            };
            let out = dedup_cdc_batch(batch, &flow);
            assert_eq!(out.rows.len(), 3);
            assert_eq!(out.rows[0].values[0], Value::Int64(1));
            assert_eq!(out.rows[1].values[0], Value::Int64(2));
            assert_eq!(out.rows[2].values[0], Value::Int64(3));
        }

        #[test]
        fn no_deletes_short_circuits() {
            // All-upsert batches should bypass dedup to keep the hot
            // path free of allocations. We assert the function
            // returns the batch unchanged (length-preserved).
            let flow = cdc_flow_with_conflict(vec!["id"]);
            let batch = Batch {
                rows: vec![row(RowOp::Upsert, 1, "a"), row(RowOp::Upsert, 1, "b")],
                next_cursor: None,
            };
            let out = dedup_cdc_batch(batch, &flow);
            // Both rows kept — second upsert overwrites the first via
            // sink upsert semantics, no need to dedup pre-write.
            assert_eq!(out.rows.len(), 2);
        }

        #[test]
        fn non_cdc_flow_skips() {
            // ColumnCursor persistence: dedup must be a no-op even
            // if the batch happens to contain a Delete.
            let flow = flow_with(CursorPersistence::ColumnCursor, Some(vec!["id"]));
            let batch = Batch {
                rows: vec![row(RowOp::Delete, 1, ""), row(RowOp::Upsert, 1, "x")],
                next_cursor: None,
            };
            let out = dedup_cdc_batch(batch, &flow);
            assert_eq!(out.rows.len(), 2);
        }

        #[test]
        fn compound_key_distinguishes_rows() {
            let flow = cdc_flow_with_conflict(vec!["id", "name"]);
            // Same id, different name → different keys, both kept
            // (last op for each key is the survivor).
            let batch = Batch {
                rows: vec![
                    row(RowOp::Upsert, 1, "a"),
                    row(RowOp::Delete, 1, "b"),
                    row(RowOp::Delete, 1, "a"),
                ],
                next_cursor: None,
            };
            let out = dedup_cdc_batch(batch, &flow);
            // Two distinct keys (1,"a") and (1,"b"). For (1,"a") the
            // last op is Delete; for (1,"b") it's Delete.
            assert_eq!(out.rows.len(), 2);
            assert!(out.rows.iter().all(|r| r.op == RowOp::Delete));
        }
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

    #[tokio::test(start_paused = true)]
    async fn shutdown_during_backoff_returns_ok() {
        // Build the source inline so we can enforce `.times(1..)`, proving
        // that read_batch was actually called (and failed) before shutdown
        // interrupted the subsequent backoff sleep.
        let mut source = crate::traits::MockSource::new();
        source.expect_cancel_safe().return_const(true);
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
    fn apply_conversions_identity_short_circuit() {
        use crate::model::{Batch, Row};
        use crate::types::DataType;
        let batch = Batch {
            rows: vec![Row::upsert(vec![Value::Int32(1), Value::Text("x".into())])],
            next_cursor: None,
        };
        let convs = vec![
            crate::model::ConversionPlan::identity(DataType::Int32),
            crate::model::ConversionPlan::identity(DataType::text()),
        ];
        let out = apply_conversions(batch, &convs).unwrap();
        assert_eq!(
            out.rows[0].values,
            vec![Value::Int32(1), Value::Text("x".into())]
        );
    }

    #[test]
    fn apply_conversions_runs_per_cell() {
        use crate::model::{Batch, Row};
        use crate::types::DataType;
        let batch = Batch {
            rows: vec![Row::upsert(vec![Value::Int16(7), Value::Int32(3)])],
            next_cursor: None,
        };
        let convs = vec![
            crate::model::ConversionPlan {
                source: DataType::Int16,
                sink: DataType::Int64,
                ctx: crate::types::ConversionContext::passthrough(),
            },
            crate::model::ConversionPlan::identity(DataType::Int32),
        ];
        let out = apply_conversions(batch, &convs).unwrap();
        assert_eq!(out.rows[0].values, vec![Value::Int64(7), Value::Int32(3)]);
    }

    #[test]
    fn apply_conversions_length_mismatch_errors() {
        use crate::model::{Batch, Row};
        use crate::types::DataType;
        let batch = Batch {
            rows: vec![Row::upsert(vec![Value::Int32(1)])],
            next_cursor: None,
        };
        let convs = vec![
            crate::model::ConversionPlan {
                source: DataType::Int32,
                sink: DataType::Int64,
                ctx: crate::types::ConversionContext::passthrough(),
            },
            crate::model::ConversionPlan {
                source: DataType::Int32,
                sink: DataType::Int64,
                ctx: crate::types::ConversionContext::passthrough(),
            },
        ];
        let res = apply_conversions(batch, &convs);
        assert!(res.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn spawn_detach_path_completes_when_source_is_not_cancel_safe() {
        // A source that opts out of cancellation safety must still
        // produce correct results in the happy path — exercises the
        // `with_spawn_detach` branch end-to-end.
        let mut source = crate::traits::MockSource::new();
        source.expect_cancel_safe().return_const(false);
        source
            .expect_build_context()
            .returning(|_| Ok(Arc::new(UnitSourceCtx)));
        let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        source.expect_read_batch().returning(move |_, _, _| {
            let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(one_row_batch())
            } else {
                Ok(crate::model::Batch::default())
            }
        });
        let flow = test_flow(source, mock_sink_ok(), mock_storage_ok());
        let (_tx, rx) = watch::channel(false);
        assert!(run(flow, RunMode::Once, rx).run().await.is_ok());
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
