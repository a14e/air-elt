//! Mongo client construction with project-wide pool / timeout settings.
//!
//! The official `mongodb` driver maintains its own internal connection
//! pool. We map our shared `PoolSettings` onto the closest equivalent
//! `ClientOptions` knobs (`connect_timeout`, `max_pool_size`,
//! `min_pool_size`, `max_idle_time`) and apply our connect timeout via
//! `tokio::time::timeout` — the driver will retry forever otherwise.
//!
//! `acquire`, `max_lifetime` and `statement` from `PoolSettings` have
//! no direct knob on the pinned `mongodb` 3.6 `ClientOptions` (no
//! `default_timeout` field — that arrived later). Per-operation caps
//! must be applied at each call site via the per-options
//! `max_time(Duration)` field (Find / Aggregate / FindOne / etc.).
//! This is load-bearing: the `mongodb` 3.x Rust driver is **not**
//! cancellation-safe — dropping a future mid-await can leave driver
//! internals inconsistent. Each mongo adapter (source / sink / storage)
//! therefore wraps its driver call in `task::detached`, which spawns
//! the work on the runtime and awaits the `JoinHandle`. Dropping the
//! handle does not abort the task, so the driver future runs to
//! completion in the background; the server's `maxTimeMS` bounds
//! runaway work. The flow runner is oblivious to cancel-safety — it
//! only wraps adapter calls in `tokio::time::timeout` + a shutdown
//! `select!`. See `task::detached`.

use std::time::Duration;

use mongodb::Client;
use mongodb::options::ClientOptions;

pub use air_elt_commons::pool_settings::PoolSettings;

use air_elt_core::error::{RuntimeError, RuntimeResult};

pub async fn connect(url: &str, settings: PoolSettings) -> RuntimeResult<Client> {
    let mut options = ClientOptions::parse(url)
        .await
        .map_err(RuntimeError::backend)?;
    options.connect_timeout = Some(settings.connect);
    options.server_selection_timeout = Some(settings.acquire);
    options.max_pool_size = Some(settings.max_connections);
    options.min_pool_size = Some(settings.min_connections);
    options.max_idle_time = Some(settings.idle);
    options.app_name = Some("air-elt".to_string());

    // `Client::with_options` is synchronous (no I/O); the driver
    // honours `connect_timeout` on the first real wire operation. No
    // outer `tokio::time::timeout` is needed here — it would be a
    // no-op around a synchronous function.
    Client::with_options(options).map_err(RuntimeError::backend)
}

pub fn database_from_url(url: &str) -> Option<String> {
    // mongodb URIs look like `mongodb://[user:pass@]host[:port]/db?opts`.
    // We only need the optional path segment.
    let trimmed = url.split('?').next().unwrap_or(url);
    let after_scheme = trimmed.split("://").nth(1)?;
    let path_start = after_scheme.find('/')?;
    let path = &after_scheme[path_start + 1..];
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

pub fn ensure_duration(d: Duration) -> Duration {
    if d.is_zero() {
        Duration::from_millis(1)
    } else {
        d
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn extracts_db_from_url() {
        assert_eq!(
            database_from_url("mongodb://h:27017/appdb"),
            Some("appdb".to_string())
        );
        assert_eq!(
            database_from_url("mongodb://h:27017/appdb?retryWrites=true"),
            Some("appdb".to_string())
        );
        assert_eq!(database_from_url("mongodb://h:27017/"), None);
        assert_eq!(database_from_url("mongodb://h:27017"), None);
    }
}
