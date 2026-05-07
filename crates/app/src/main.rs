use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;
use tokio::sync::watch;
use tracing::{error, info};

use air_elt_app::App;
use air_elt_commons::tracing_init;
use air_elt_core::config::model::RootConfig;

mod signal;

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
    command: Option<Command>,
}

/// CLI default when `--config` is omitted: probe the working directory for
/// `config.toml`, then `config.yml`, then `config.yaml`. Returns the first
/// existing file. If none exist, falls back to `./config.toml` so the
/// downstream loader emits a clear "file not found" diagnostic against the
/// canonical name.
fn default_config_path() -> PathBuf {
    for candidate in ["./config.toml", "./config.yml", "./config.yaml"] {
        let p = PathBuf::from(candidate);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from("./config.toml")
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
    /// Print the source / sink / storage kinds wired into the registry.
    ListKinds,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_init::init();
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Validate { config }) => {
            let app = App::from_path(&config)?;
            app.validate().await?;
            info!("validation successful");
            Ok(())
        }
        Some(Command::Migrate { config }) => {
            let app = App::from_path(&config)?;
            app.migrate().await?;
            info!("migrations applied");
            Ok(())
        }
        Some(Command::Run { config, once }) => run(&config, once).await,
        Some(Command::ListKinds) => {
            // `list-kinds` reports what the binary is wired with, so no config
            // is needed — feed an empty `RootConfig` to reuse `App`'s surface.
            let kinds = App::from_config(RootConfig::default()).list_kinds();
            println!("sources:");
            for k in &kinds.sources {
                println!("  - {k}");
            }
            println!("sinks:");
            for k in &kinds.sinks {
                println!("  - {k}");
            }
            println!("storages:");
            for k in &kinds.storages {
                println!("  - {k}");
            }
            Ok(())
        }
        None => run(&default_config_path(), false).await,
    }
}

async fn run(config: &Path, once: bool) -> anyhow::Result<()> {
    let app = App::from_path(config)?;
    if once {
        return match app.run_once().await {
            Ok(()) => Ok(()),
            Err(e) => {
                error!(error = %e, "engine failed");
                Err(e)
            }
        };
    }

    let (tx, rx) = watch::channel(false);
    let shutdown = tokio::spawn(async move {
        signal::wait_for_shutdown(&tx).await;
    });
    let result = app.run_daemon(rx).await;
    shutdown.abort();
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            error!(error = %e, "engine failed");
            Err(e)
        }
    }
}
