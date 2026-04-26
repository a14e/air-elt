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
            self.cursor = with_timeout(
                &self.flow,
                "load_cursor",
                self.flow.storage.load_cursor(&self.flow.name),
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
        let batch = with_timeout(
            &self.flow,
            "read_batch",
            self.flow
                .source
                .read_batch(&self.flow.read_spec, src_ctx, self.cursor.as_ref()),
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
        let report = match with_timeout(
            &self.flow,
            "write_batch",
            self.flow
                .sink
                .write_batch(&self.flow.write_spec, sink_ctx, batch),
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
            if let Err(e) = with_timeout(
                &self.flow,
                "save_cursor",
                self.flow.storage.save_cursor(&self.flow.name, next),
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
    conversions: &[(crate::types::DataType, crate::types::DataType)],
) -> RuntimeResult<Batch> {
    if conversions.is_empty() || conversions.iter().all(|(s, d)| s == d) {
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
        for (slot, (src_dt, sink_dt)) in row.values.iter_mut().zip(conversions.iter()) {
            if src_dt == sink_dt {
                continue;
            }
            let owned = std::mem::replace(slot, Value::Null);
            *slot = convert::convert(owned, src_dt, sink_dt)?;
        }
    }
    Ok(batch)
}

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
            (DataType::Int32, DataType::Int32),
            (DataType::text(), DataType::text()),
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
            (DataType::Int16, DataType::Int64),
            (DataType::Int32, DataType::Int32),
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
            (DataType::Int32, DataType::Int64),
            (DataType::Int32, DataType::Int64),
        ];
        let res = apply_conversions(batch, &convs);
        assert!(res.is_err());
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
