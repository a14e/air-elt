use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::model::{Batch, CursorState};
use crate::validation::pipeline::ResolvedFlow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Daemon,
    Once,
}

const BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const BACKOFF_CAP: Duration = Duration::from_secs(3600);
const BACKOFF_MULTIPLIER: u32 = 4;

pub struct FlowRunner {
    flow: Arc<ResolvedFlow>,
    mode: RunMode,
    shutdown: watch::Receiver<bool>,
    cursor: Option<CursorState>,
    backoff: Duration,
}

impl FlowRunner {
    pub fn new(flow: Arc<ResolvedFlow>, mode: RunMode, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            flow,
            mode,
            shutdown,
            cursor: None,
            backoff: BACKOFF_INITIAL,
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

        if *self.shutdown.borrow() {
            info!(flow = %self.flow.name, "shutdown signalled");
            return Ok(true);
        }

        let batch = with_timeout(
            &self.flow,
            "read_batch",
            self.flow
                .source
                .read_batch(&self.flow.read_spec, self.cursor.as_ref()),
            &mut self.shutdown,
        )
        .await?;
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
        let report = match with_timeout(
            &self.flow,
            "write_batch",
            self.flow.sink.write_batch(&self.flow.write_spec, batch),
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

async fn with_timeout<F, T>(
    flow: &ResolvedFlow,
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

pub async fn run_all_flows(
    flows: Vec<Arc<ResolvedFlow>>,
    mode: RunMode,
    shutdown: watch::Receiver<bool>,
) -> RuntimeResult<()> {
    info!(flows = flows.len(), ?mode, "starting flow runners");

    let mut handles = Vec::with_capacity(flows.len());
    for flow in flows {
        let rx = shutdown.clone();
        let flow_name = flow.name.clone();
        handles.push(tokio::spawn(async move {
            let runner = FlowRunner::new(flow, mode, rx);
            let result = runner.run().await;
            (flow_name, result)
        }));
    }

    let mut first_error: Option<RuntimeError> = None;
    for handle in handles {
        match handle.await {
            Ok((name, Ok(()))) => info!(flow = %name, "flow completed"),
            Ok((name, Err(e))) => {
                error!(flow = %name, error = %e, "flow failed");
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
            Err(e) => {
                error!(error = %e, "flow task panicked");
                if first_error.is_none() {
                    first_error = Some(RuntimeError::Other(e.to_string()));
                }
            }
        }
    }

    match first_error {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::config::model::CursorOrder;
    use crate::model::{
        Batch, CursorFieldValue, CursorState, ReadSpec, Row, WriteReport, WriteSpec,
    };
    use crate::traits::{MockSink, MockSource, MockStorage};
    use crate::types::value::Value;

    fn one_row_batch() -> Batch {
        Batch {
            rows: vec![Row {
                values: vec![Value::Int64(1)],
            }],
            next_cursor: Some(CursorState::new(vec![CursorFieldValue {
                name: "id".into(),
                value: Value::Int64(1),
            }])),
        }
    }

    fn mock_source_ok() -> MockSource {
        let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut s = MockSource::new();
        s.expect_read_batch().returning(move |_, _| {
            let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(one_row_batch())
            } else {
                Ok(Batch::default())
            }
        });
        s
    }

    fn mock_source_empty() -> MockSource {
        let mut s = MockSource::new();
        s.expect_read_batch().returning(|_, _| Ok(Batch::default()));
        s
    }

    fn mock_source_no_cursor() -> MockSource {
        let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut s = MockSource::new();
        s.expect_read_batch().returning(move |_, _| {
            let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                Ok(Batch {
                    rows: vec![Row {
                        values: vec![Value::Int64(1)],
                    }],
                    next_cursor: None,
                })
            } else {
                Ok(Batch::default())
            }
        });
        s
    }

    fn mock_source_failing(times: u32) -> MockSource {
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(times));
        let call = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut s = MockSource::new();
        s.expect_read_batch().returning(move |_, _| {
            let remaining = counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            if remaining > 0 {
                Err(RuntimeError::Other("source boom".into()))
            } else {
                let n = call.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if n == 0 {
                    Ok(one_row_batch())
                } else {
                    Ok(Batch::default())
                }
            }
        });
        s
    }

    fn mock_sink_ok() -> MockSink {
        let mut s = MockSink::new();
        s.expect_write_batch().returning(|_, batch| {
            Ok(WriteReport {
                rows_written: batch.rows.len() as u64,
            })
        });
        s
    }

    fn mock_storage_ok() -> MockStorage {
        let mut s = MockStorage::new();
        s.expect_load_cursor().returning(|_| Ok(None));
        s.expect_save_cursor().returning(|_, _| Ok(()));
        s
    }

    fn mock_storage_save_fails() -> MockStorage {
        let mut s = MockStorage::new();
        s.expect_load_cursor().returning(|_| Ok(None));
        s.expect_save_cursor()
            .returning(|_, _| Err(RuntimeError::Other("storage boom".into())));
        s
    }

    fn test_flow_named(
        name: &str,
        source: MockSource,
        sink: MockSink,
        storage: MockStorage,
    ) -> Arc<ResolvedFlow> {
        Arc::new(ResolvedFlow {
            name: name.into(),
            source: Arc::new(source),
            sink: Arc::new(sink),
            storage: Arc::new(storage),
            read_spec: ReadSpec {
                columns: vec!["id".into()],
                table: "public.t".into(),
                cursor_fields: vec!["id".into()],
                cursor_order: CursorOrder::Asc,
                limit: 1,
            },
            write_spec: WriteSpec {
                columns: vec!["id".into()],
                table: "public.t".into(),
            },
            interval: Duration::from_millis(10),
            query_timeout: Duration::from_secs(5),
        })
    }

    fn test_flow(source: MockSource, sink: MockSink, storage: MockStorage) -> Arc<ResolvedFlow> {
        test_flow_named("test_flow", source, sink, storage)
    }

    fn run(flow: Arc<ResolvedFlow>, mode: RunMode, rx: watch::Receiver<bool>) -> FlowRunner {
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

    #[tokio::test(start_paused = true)]
    async fn run_all_flows_collects_first_error() {
        let ok_flow = test_flow(mock_source_ok(), mock_sink_ok(), mock_storage_ok());
        let fail_flow = test_flow_named(
            "failing",
            mock_source_failing(1),
            mock_sink_ok(),
            mock_storage_ok(),
        );
        let (_tx, rx) = watch::channel(false);
        assert!(
            run_all_flows(vec![ok_flow, fail_flow], RunMode::Once, rx)
                .await
                .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn run_all_flows_all_ok() {
        let f1 = test_flow(mock_source_ok(), mock_sink_ok(), mock_storage_ok());
        let f2 = test_flow_named(
            "flow_2",
            mock_source_ok(),
            mock_sink_ok(),
            mock_storage_ok(),
        );
        let (_tx, rx) = watch::channel(false);
        assert!(run_all_flows(vec![f1, f2], RunMode::Once, rx).await.is_ok());
    }
}
