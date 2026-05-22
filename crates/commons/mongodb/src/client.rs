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
//!
//! CMAP wiring: the caller passes in a strong `Arc<MongoPoolStatsReader>`.
//! We register a CMAP event handler on `ClientOptions` that maps each
//! pool-lifecycle event to a `MongoPoolStatsReader::on_*` call (atomic
//! adds — cheap on the driver's CMAP thread). Mongo's CMAP separates
//! `ConnectionReady` (conn enters idle pool) from `CheckedOut` (conn
//! handed to caller), so the active/idle atomics reflect the true pool
//! state on every transition. The collector reads the stats reader once
//! per scrape via `PoolStatsReader::read()`.

use std::sync::Arc;
use std::time::Duration;

use mongodb::Client;
use mongodb::event::EventHandler;
use mongodb::event::cmap::{CmapEvent, ConnectionClosedReason};
use mongodb::options::ClientOptions;

pub use air_elt_commons::pool_settings::PoolSettings;

use air_elt_core::error::{RuntimeError, RuntimeResult};

use crate::pool_stats_reader::MongoPoolStatsReader;

pub async fn connect(
    url: &str,
    settings: PoolSettings,
    reader: Arc<MongoPoolStatsReader>,
) -> RuntimeResult<Client> {
    let mut options = ClientOptions::parse(url)
        .await
        .map_err(RuntimeError::backend)?;
    options.connect_timeout = Some(settings.connect);
    options.server_selection_timeout = Some(settings.acquire);
    options.max_pool_size = Some(settings.max_connections);
    options.min_pool_size = Some(settings.min_connections);
    options.max_idle_time = Some(settings.idle);
    options.app_name = Some("air-elt".to_string());

    options.cmap_event_handler = Some(cmap_handler(reader));

    // `Client::with_options` is synchronous (no I/O); the driver
    // honours `connect_timeout` on the first real wire operation. No
    // outer `tokio::time::timeout` is needed here — it would be a
    // no-op around a synchronous function.
    Client::with_options(options).map_err(RuntimeError::backend)
}

/// Map mongo CMAP events onto stats-reader transitions. The driver
/// invokes the callback synchronously on its CMAP thread, so the
/// closure must be cheap and panic-free — each `reader.on_*` is a
/// single atomic `fetch_add` / `fetch_update`.
///
/// Transition table:
/// * `ConnectionReady` — fresh conn joined the idle pool.
/// * `ConnectionCheckedOut` — idle conn handed to caller.
/// * `ConnectionCheckedIn` — caller returned conn to idle pool.
/// * `ConnectionClosed { reason }`:
///   * `Idle` / `Stale` — closed from idle (lifecycle evictions;
///     in-use conns aren't interrupted by `pool.clear` unless
///     `interrupt_in_use_connections=true`, which we don't set).
///   * `Error` / `Dropped` — closed from active (errored mid-op).
///   * `PoolClosed` — `Pool::close()` is invoked at shutdown and
///     sweeps **both** idle and active conns; mapping it to either
///     bucket would drive one slot negative. We ignore it — the
///     metric is academic at shutdown anyway (the scrape that
///     captures shutdown is one of the last ones, and the registry
///     is being torn down on the next breath).
///
/// Other CMAP variants (`PoolCreated`/`PoolReady`/`PoolCleared`/
/// `PoolClosed`/`ConnectionCreated`/`ConnectionCheckoutStarted`/
/// `ConnectionCheckoutFailed`) carry no state-transition signal we
/// track and are ignored.
fn cmap_handler(reader: Arc<MongoPoolStatsReader>) -> EventHandler<CmapEvent> {
    EventHandler::callback(move |event| match event {
        CmapEvent::ConnectionReady(_) => reader.on_pool_filled(),
        CmapEvent::ConnectionCheckedOut(_) => reader.on_idle_acquired(),
        CmapEvent::ConnectionCheckedIn(_) => reader.on_released_to_idle(),
        CmapEvent::ConnectionClosed(ev) => match ev.reason {
            ConnectionClosedReason::Idle | ConnectionClosedReason::Stale => {
                reader.on_closed_from_idle()
            }
            ConnectionClosedReason::Error | ConnectionClosedReason::Dropped => {
                reader.on_closed_from_active()
            }
            // `PoolClosed` sweeps both states at shutdown; routing to
            // either would drive the counter negative. `Unset` only
            // appears in driver test fixtures.
            _ => {}
        },
        _ => {}
    })
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
