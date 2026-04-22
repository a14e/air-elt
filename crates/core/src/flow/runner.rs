use std::sync::Arc;

use tokio::sync::watch;
use tokio::time::sleep;
use tracing::{error, info, warn};

use crate::error::{RuntimeError, RuntimeResult};
use crate::flow::state::CursorState;
use crate::traits::Batch;
use crate::validation::pipeline::ResolvedFlow;

/// How the caller wants the runner to behave.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    /// Loop forever: read → write → save cursor → sleep interval when idle.
    Daemon,
    /// Drain the source once (repeatedly pull while batches are full) and exit.
    Once,
}

/// Run a single flow. Shutdown is signalled by `shutdown.changed()` flipping to true.
pub async fn run_flow(
    flow: Arc<ResolvedFlow>,
    mode: RunMode,
    mut shutdown: watch::Receiver<bool>,
) -> RuntimeResult<()> {
    let mut cursor = with_timeout(
        &flow,
        "load_cursor",
        flow.storage.load_cursor(&flow.name),
        &mut shutdown,
    )
    .await?;
    info!(flow = %flow.name, has_cursor = cursor.is_some(), "flow started");

    loop {
        if *shutdown.borrow() {
            info!(flow = %flow.name, "shutdown signalled");
            return Ok(());
        }

        let batch = with_timeout(
            &flow,
            "read_batch",
            flow.source.read_batch(&flow.read_spec, cursor.as_ref()),
            &mut shutdown,
        )
        .await?;
        let batch_size = batch.rows.len();

        if batch_size == 0 {
            if matches!(mode, RunMode::Once) {
                info!(flow = %flow.name, "drain complete");
                return Ok(());
            }
            // Idle — wait for either the configured interval or a shutdown signal.
            tokio::select! {
                _ = sleep(flow.interval) => {}
                _ = shutdown.changed() => {
                    return Ok(());
                }
            }
            continue;
        }

        write_and_commit(&flow, &batch, &mut cursor, &mut shutdown).await?;

        // If the source returned a partial batch, the next read will be empty
        // and we either exit (Once) or sleep (Daemon) on the next iteration.
        if batch_size < flow.read_spec.limit && matches!(mode, RunMode::Once) {
            return Ok(());
        }
    }
}

async fn write_and_commit(
    flow: &ResolvedFlow,
    batch: &Batch,
    cursor: &mut Option<CursorState>,
    shutdown: &mut watch::Receiver<bool>,
) -> RuntimeResult<()> {
    let report = match with_timeout(
        flow,
        "write_batch",
        flow.sink.write_batch(&flow.write_spec, batch),
        shutdown,
    )
    .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(flow = %flow.name, error = %e, "write_batch failed");
            return Err(e);
        }
    };
    info!(
        flow = %flow.name,
        rows = report.rows_written,
        "batch written"
    );

    if let Some(next) = &batch.next_cursor {
        if let Err(e) = with_timeout(
            flow,
            "save_cursor",
            flow.storage.save_cursor(&flow.name, next),
            shutdown,
        )
        .await
        {
            // Why: cursor loss = re-emit rows on restart. If we cannot persist,
            // we must abort the flow — a later retry will pick up from the last
            // successfully saved cursor rather than silently double-writing.
            error!(flow = %flow.name, error = %e, "save_cursor failed; flow will abort to avoid drift");
            return Err(e);
        }
        *cursor = Some(next.clone());
    } else {
        warn!(
            flow = %flow.name,
            "source returned a batch without a next cursor; skipping cursor save"
        );
    }
    Ok(())
}

/// Wrap an async DB-touching future in `operation_timeout` plus a
/// `shutdown.changed()` race. Propagates `RuntimeError::Other` on both timeout
/// and shutdown-cancel so the caller can distinguish from a backend failure.
///
/// Why a single helper: `read_batch`/`write_batch`/`save_cursor`/`load_cursor`
/// all share the same "bail on shutdown or after N seconds" rule — centralising
/// it prevents drift.
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
        res = tokio::time::timeout(flow.operation_timeout, fut) => match res {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(RuntimeError::Other(format!(
                "flow {:?} operation {op:?} timed out after {:?}",
                flow.name, flow.operation_timeout
            ))),
        },
        _ = shutdown.changed() => Err(RuntimeError::Other(format!(
            "flow {:?} operation {op:?} cancelled by shutdown",
            flow.name
        ))),
    }
}

/// Spawn each resolved flow as its own tokio task and wait for them all.
///
/// Why "first error wins, others logged": a single failure shouldn't hide the
/// rest of the failures in the output, but we need to return one concrete
/// error to the caller. Operators see every failure in logs and can act on
/// them in parallel.
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
            let result = run_flow(flow, mode, rx).await;
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
