use std::path::PathBuf;

use tokio::sync::watch;
use tracing::{error, info};

use air_elt_core::config::loader;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::validation::pipeline::{assemble, validate};

use air_elt_app::registry::build_registry;

use crate::signal::wait_for_shutdown;

pub async fn cmd_validate(config: PathBuf) -> anyhow::Result<()> {
    let root = loader::load(&config)?;
    let registry = build_registry();
    let assembled = assemble(&root, &registry).await?;
    let flows = validate(assembled).await?;
    info!(flow_count = flows.len(), "validation successful");
    Ok(())
}

pub async fn cmd_migrate(config: PathBuf) -> anyhow::Result<()> {
    let root = loader::load(&config)?;
    let registry = build_registry();
    let assembled = assemble(&root, &registry).await?;
    let flows = validate(assembled).await?;
    for flow in &flows {
        flow.storage.migrate().await?;
    }
    info!(storage_count = flows.len(), "migrations applied");
    Ok(())
}

pub async fn cmd_run(config: PathBuf, once: bool) -> anyhow::Result<()> {
    let root = loader::load(&config)?;
    let registry = build_registry();
    let assembled = assemble(&root, &registry).await?;
    let flows = validate(assembled).await?;

    let (tx, rx) = watch::channel(false);
    let mode = if once { RunMode::Once } else { RunMode::Daemon };

    let engine = tokio::spawn(FlowEngine::new(flows, mode, rx).run());

    if matches!(mode, RunMode::Daemon) {
        wait_for_shutdown(&tx).await;
    }

    match engine.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            error!(error = %e, "engine failed");
            Err(anyhow::anyhow!("engine: {e}"))
        }
        Err(e) => Err(anyhow::anyhow!("engine task panicked: {e}")),
    }
}
