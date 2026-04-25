use std::sync::Once;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt;
use tracing_subscriber::prelude::*;

static INIT: Once = Once::new();

/// Initialise the global tracing subscriber. Idempotent — safe to call multiple
/// times (e.g. from tests + from `main`).
///
/// Defaults: level `info`, overridable via `RUST_LOG` / `AIR_ELT_LOG`.
pub fn init() {
    INIT.call_once(|| {
        let filter = EnvFilter::try_from_env("AIR_ELT_LOG")
            .or_else(|_| EnvFilter::try_from_default_env())
            .unwrap_or_else(|_| EnvFilter::new("info"));

        let layer = fmt::layer()
            .with_target(true)
            .with_thread_ids(false)
            .with_thread_names(false);

        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(layer)
            .try_init();
    });
}
