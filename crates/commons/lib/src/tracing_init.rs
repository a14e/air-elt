use std::io;
use std::sync::Once;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;

use crate::bool_flag;

static INIT: Once = Once::new();

/// Initialise the global tracing subscriber. Idempotent — safe to call
/// multiple times (e.g. from tests + from `main`). Only the first call
/// installs the subscriber and owns the worker guard; later calls return
/// `None`.
///
/// The returned `WorkerGuard` drives the async background writer: dropping it
/// flushes the queue and terminates the worker thread. Bind it for the entire
/// process lifetime — `let _g = tracing_init::init();` — otherwise queued log
/// lines are lost. `#[must_use]` makes a bare `let _ = init();` (which would
/// drop the guard immediately) a compiler warning.
///
/// Env-driven knobs (boolean values parsed via [`bool_flag::parse`]):
/// - `AIR_ELT_LOG` / `RUST_LOG` — level filter (default `info`).
/// - `AIR_ELT_SYNC_LOGGING=true` — fall back to synchronous writes (no
///   background worker). Useful when you cannot afford to lose logs around
///   startup / crashes.
/// - `AIR_ELT_JSON_LOGGING=true` — emit logs as JSON instead of the default
///   human-friendly text format.
#[must_use = "drop flushes the async worker; bind it for the program's lifetime"]
pub fn init() -> Option<WorkerGuard> {
    let mut guard = None;
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_env("AIR_ELT_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new("info"));

        let json = bool_flag::from_env("AIR_ELT_JSON_LOGGING", false);
        let sync = bool_flag::from_env("AIR_ELT_SYNC_LOGGING", false);

        if sync {
            install(filter, json, io::stdout);
        } else {
            let (writer, worker_guard) = tracing_appender::non_blocking(io::stdout());
            install(filter, json, writer);
            guard = Some(worker_guard);
        }
    });
    guard
}

fn install<W>(filter: EnvFilter, json: bool, writer: W)
where
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let base = fmt::layer().with_target(true).with_writer(writer);
    let registry = tracing_subscriber::registry().with(filter);
    if json {
        registry
            .with(base.json())
            .try_init()
            .expect("tracing subscriber must initialise on first call");
    } else {
        registry
            .with(base)
            .try_init()
            .expect("tracing subscriber must initialise on first call");
    }
}
