use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{Batch, CursorState, FlowState, SinkCtx, SourceCtx};
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
            let fut = async move { storage.load_cursor(&flow_name).await };
            self.cursor = run_op(
                &self.flow,
                "load_cursor",
                fut,
                cancel_safe,
                &mut self.shutdown,
            )
            .await?;
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
            let next_owned = next.clone();
            let cancel_safe = storage.cancel_safe();
            let fut = async move { storage.save_cursor(&flow_name, &next_owned).await };
            if let Err(e) = run_op(
                &self.flow,
                "save_cursor",
                fut,
                cancel_safe,
                &mut self.shutdown,
            )
            .await
            {
                error!(flow = %self.flow.name, error = %e, "save_cursor failed; flow will abort to avoid drift");
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
    use crate::flow::test_support::*;

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
            rows: vec![Row {
                values: vec![Value::Int32(1), Value::Text("x".into())],
            }],
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
            rows: vec![Row {
                values: vec![Value::Int16(7), Value::Int32(3)],
            }],
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
            rows: vec![Row {
                values: vec![Value::Int32(1)],
            }],
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
