use std::time::Duration;

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{Batch, CursorState, FlowState, SinkCtx, SourceCtx};

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
    source_ctx: Option<Box<dyn SourceCtx>>,
    sink_ctx: Option<Box<dyn SinkCtx>>,
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
            self.source_ctx = Some(self.flow.source.init_context(&self.flow.read_spec).await?);
        }
        if self.sink_ctx.is_none() {
            self.sink_ctx = Some(self.flow.sink.init_context(&self.flow.write_spec).await?);
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

        // Functional-style context threading: take ownership, pass through
        // read_batch, get it back. If timeout/shutdown cancels the future,
        // ctx is dropped — ensure_contexts() will recreate it on next tick.
        let src_ctx = self.source_ctx.take().expect("ensured by ensure_contexts");
        let (batch, src_ctx) = with_timeout(
            &self.flow,
            "read_batch",
            self.flow
                .source
                .read_batch(&self.flow.read_spec, src_ctx, self.cursor.as_ref()),
            &mut self.shutdown,
        )
        .await?;
        self.source_ctx = Some(src_ctx);

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
        // See source_ctx comment in tick() — same cancellation-safety caveat.
        let sink_ctx = self.sink_ctx.take().expect("ensured by ensure_contexts");
        let (report, sink_ctx) = match with_timeout(
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
        self.sink_ctx = Some(sink_ctx);

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
            Err(_) => Err(RuntimeError::Other(format!(
                "flow {:?} operation {op:?} timed out after {:?}",
                flow.name, flow.query_timeout
            ))),
        },
        _ = shutdown.changed() => Err(RuntimeError::Other(format!(
            "flow {:?} operation {op:?} cancelled by shutdown",
            flow.name
        ))),
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
        let flow = test_flow(mock_source_failing(100), mock_sink_ok(), mock_storage_ok());
        let (tx, rx) = watch::channel(false);
        let handle =
            tokio::spawn(async move { FlowRunner::new(flow, RunMode::Daemon, rx).run().await });
        tokio::time::advance(Duration::from_millis(500)).await;
        tx.send(true).unwrap();
        assert!(handle.await.unwrap().is_ok());
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
