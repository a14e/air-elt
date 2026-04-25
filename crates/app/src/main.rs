use std::path::PathBuf;

use clap::{Parser, Subcommand};
use mimalloc::MiMalloc;

use air_elt_commons::tracing_init;

mod commands;
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

fn default_config_path() -> PathBuf {
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_init::init();
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Validate { config }) => commands::cmd_validate(config).await,
        Some(Command::Migrate { config }) => commands::cmd_migrate(config).await,
        Some(Command::Run { config, once }) => commands::cmd_run(config, once).await,
        None => commands::cmd_run(default_config_path(), false).await,
    }
}
