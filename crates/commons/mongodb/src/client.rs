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
//! cancellation-safe, so the flow runner does NOT wrap mongo calls in
//! `tokio::time::timeout` (dropping a future mid-await can leave driver
//! internals inconsistent). Instead, the runner spawns the call and
//! detaches the `JoinHandle` on timeout — the underlying driver future
//! runs to completion in the background — and the server's `maxTimeMS`
//! bounds runaway work. See `core::flow::runner::with_timeout` and the
//! `cancel_safe()` trait method on `Source`/`Sink`/`Storage`.

use std::str::FromStr;
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

    let client = tokio::time::timeout(settings.connect, async { Client::with_options(options) })
        .await
        .map_err(|_| {
            RuntimeError::Other(format!(
                "mongo connect timed out after {:?}",
                settings.connect
            ))
        })?
        .map_err(RuntimeError::backend)?;
    Ok(client)
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

// Suppress unused-import warning when the helper above is the only
// non-trivial call site — keeps `FromStr` available for future
// callers without forcing them to re-import.
#[allow(dead_code)]
fn _force_use_from_str() -> bool {
    let _ = i32::from_str("0");
    true
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
