use tokio::sync::watch;
use tracing::{error, info};

use crate::error::{RuntimeError, RuntimeResult};
use crate::flow::runner::{FlowRunner, RunMode};
use crate::model::FlowState;

pub struct FlowEngine {
    flows: Vec<FlowState>,
    mode: RunMode,
    shutdown: watch::Receiver<bool>,
}

impl FlowEngine {
    pub fn new(flows: Vec<FlowState>, mode: RunMode, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            flows,
            mode,
            shutdown,
        }
    }

    pub async fn run(self) -> RuntimeResult<()> {
        info!(flows = self.flows.len(), ?self.mode, "starting flow engine");

        let mut handles = Vec::with_capacity(self.flows.len());
        for flow in self.flows {
            let rx = self.shutdown.clone();
            let flow_name = flow.name.clone();
            let mode = self.mode;
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::flow::test_support::*;

    #[tokio::test(start_paused = true)]
    async fn engine_collects_first_error() {
        let ok_flow = test_flow(mock_source_ok(), mock_sink_ok(), mock_storage_ok());
        let fail_flow = test_flow_named(
            "failing",
            mock_source_failing(1),
            mock_sink_ok(),
            mock_storage_ok(),
        );
        let (_tx, rx) = watch::channel(false);
        let engine = FlowEngine::new(vec![ok_flow, fail_flow], RunMode::Once, rx);
        let err = engine.run().await.unwrap_err();
        assert!(
            err.to_string().contains("source boom"),
            "expected 'source boom', got: {err}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn engine_all_ok() {
        let f1 = test_flow(mock_source_ok(), mock_sink_ok(), mock_storage_ok());
        let f2 = test_flow_named(
            "flow_2",
            mock_source_ok(),
            mock_sink_ok(),
            mock_storage_ok(),
        );
        let (_tx, rx) = watch::channel(false);
        let engine = FlowEngine::new(vec![f1, f2], RunMode::Once, rx);
        assert!(engine.run().await.is_ok());
    }
}
