use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use tokio::signal;
use tokio::sync::watch;
use tracing::{error, info};

use air_elt_app::registry::build_registry;
use air_elt_commons::tracing_init;
use air_elt_core::config::loader;
use air_elt_core::flow::runner::{RunMode, run_all_flows};
use air_elt_core::validation::pipeline::validate as run_validation;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Debug, Parser)]
#[command(
    name = "air-elt",
    version,
    about = "Air Elt — declarative ELT pipelines"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Parse, connect, and validate the config end-to-end.
    Validate {
        #[arg(long, short)]
        config: PathBuf,
    },
    /// Run `Storage::migrate` for every declared storage.
    Migrate {
        #[arg(long, short)]
        config: PathBuf,
    },
    /// Run all flows. Defaults to daemon mode with SIGTERM-driven shutdown.
    Run {
        #[arg(long, short)]
        config: PathBuf,
        /// Drain once and exit (useful for batch jobs and e2e tests).
        #[arg(long)]
        once: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_init::init();
    let cli = Cli::parse();

    match cli.command {
        Command::Validate { config } => cmd_validate(config).await,
        Command::Migrate { config } => cmd_migrate(config).await,
        Command::Run { config, once } => cmd_run(config, once).await,
    }
}

async fn cmd_validate(config: PathBuf) -> anyhow::Result<()> {
    let root = loader::load(&config)?;
    let registry = build_registry();
    let flows = run_validation(&root, &registry).await?;
    info!(flow_count = flows.len(), "validation successful");
    Ok(())
}

async fn cmd_migrate(config: PathBuf) -> anyhow::Result<()> {
    let root = loader::load(&config)?;
    let registry = build_registry();
    let flows = run_validation(&root, &registry).await?;
    for flow in &flows {
        flow.storage.migrate().await?;
    }
    info!(storage_count = flows.len(), "migrations applied");
    Ok(())
}

async fn cmd_run(config: PathBuf, once: bool) -> anyhow::Result<()> {
    let root = loader::load(&config)?;
    let registry = build_registry();
    let flows = run_validation(&root, &registry).await?;
    let flows: Vec<_> = flows.into_iter().map(Arc::new).collect();

    let (tx, rx) = watch::channel(false);
    let mode = if once { RunMode::Once } else { RunMode::Daemon };

    let runner = tokio::spawn(run_all_flows(flows, mode, rx));

    if matches!(mode, RunMode::Daemon) {
        wait_for_shutdown(&tx).await;
    }

    match runner.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            error!(error = %e, "runner failed");
            Err(anyhow::anyhow!("runner: {e}"))
        }
        Err(e) => Err(anyhow::anyhow!("runner task panicked: {e}")),
    }
}

async fn wait_for_shutdown(tx: &watch::Sender<bool>) {
    let ctrl_c = async {
        // Why: signal registration can fail in restrictive seccomp profiles.
        // Logging + exit(1) is more operator-friendly than an `expect`-panic
        // mid-shutdown-hotpath: the process fails early with a clear reason
        // instead of dumping a stack trace.
        signal::ctrl_c().await.unwrap_or_else(|e| {
            error!(error = ?e, "ctrl_c handler install failed");
            std::process::exit(1);
        });
    };

    #[cfg(unix)]
    let term = async {
        let mut stream = signal::unix::signal(signal::unix::SignalKind::terminate())
            .unwrap_or_else(|e| {
                error!(error = ?e, "SIGTERM handler install failed");
                std::process::exit(1);
            });
        stream.recv().await;
    };

    #[cfg(not(unix))]
    let term = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received ctrl_c"),
        _ = term => info!("received SIGTERM"),
    }
    // Why: send fails only if every receiver was dropped, which is the
    // expected state if all flows already stopped on their own. Debug-log and
    // move on — operator doesn't need to see this at info.
    if tx.send(true).is_err() {
        tracing::debug!("shutdown channel already closed — flows likely completed on their own");
    }
}
