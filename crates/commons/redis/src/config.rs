//! Pool configuration.
//!
//! Two scopes mirror the rest of the workspace:
//!
//! * [`RedisPoolConfig`] — the raw serde shape nested under the redis
//!   sink's `config = { url, pool = { ... } }` table. All fields optional;
//!   omitting `pool` yields all defaults.
//! * [`RedisPoolSettings`] — the resolved, all-concrete struct the pool
//!   runs on, built once via [`RedisPoolSettings::create`].
//!
//! The surface is a classic connection pool: a single `max-connections`
//! width plus the three checkout timeouts `deadpool` exposes. There is no
//! width × depth multiplexing knob — the pool hands out one connection per
//! checkout (a whole-batch pipeline rides one connection), so a second
//! "depth" dimension would be meaningless.

use std::time::Duration;

use air_elt_commons::interval;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default number of pooled connections.
const DEFAULT_MAX_CONNECTIONS: u32 = 10;
/// Hard cap on the pool size — mirrors the SQL pools' 100-connection
/// ceiling so one sink can't open an unbounded number of sockets.
const MAX_CONNECTIONS_CAP: u32 = 100;
/// TCP connect timeout for dialing a new connection (deadpool `create`).
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// Max time `acquire` waits for a free connection when the pool is
/// saturated (deadpool `wait`).
const DEFAULT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on the health-check (`PING`) run when an idle connection is
/// re-checked out (deadpool `recycle`).
const DEFAULT_RECYCLE_TIMEOUT: Duration = Duration::from_secs(5);

/// Raw serde shape of `pool = { ... }`. Every field is consumed by
/// [`RedisPoolSettings::create`] — no future-proofing fields.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RedisPoolConfig {
    /// Number of pooled connections. Default 10, clamped at 100.
    #[serde(default)]
    pub max_connections: Option<u32>,
    /// TCP connect timeout for dialing a new connection. Default 5s.
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub connect_timeout: Option<Duration>,
    /// Max time `acquire` waits for a free connection while the pool is
    /// saturated. Default 10s.
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub acquire_timeout: Option<Duration>,
    /// Bound on the health-check probe run when an idle connection is
    /// recycled on checkout. Default 5s.
    #[serde(
        default,
        deserialize_with = "interval::deserialize_opt",
        serialize_with = "interval::serialize_opt"
    )]
    pub recycle_timeout: Option<Duration>,
}

/// Configuration rejected before any I/O.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RedisPoolConfigError {
    /// `max-connections = 0` would make the pool unable to ever serve a
    /// command. Reject at build time (mirrors the SQL pools' zero-max
    /// rejection).
    #[error("pool max-connections must be ≥ 1 (got 0)")]
    ZeroMaxConnections,
}

/// Resolved pool settings. Every field concrete; invariants checked at
/// construction.
#[derive(Debug, Clone, Copy)]
pub struct RedisPoolSettings {
    /// Number of pooled connections (the pool's `max_size`). Also the
    /// value the redis sink reports from `Sink::max_connections()` so the
    /// runtime concurrency semaphore matches the pool's true capacity.
    pub max_connections: u32,
    pub connect_timeout: Duration,
    pub acquire_timeout: Duration,
    pub recycle_timeout: Duration,
}

impl RedisPoolSettings {
    /// Resolve raw config into concrete settings, applying defaults and
    /// the connection-count clamp.
    pub fn create(cfg: &RedisPoolConfig) -> Result<Self, RedisPoolConfigError> {
        let max_connections = cfg.max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS);
        if max_connections == 0 {
            return Err(RedisPoolConfigError::ZeroMaxConnections);
        }
        Ok(Self {
            max_connections: max_connections.min(MAX_CONNECTIONS_CAP),
            connect_timeout: cfg.connect_timeout.unwrap_or(DEFAULT_CONNECT_TIMEOUT),
            acquire_timeout: cfg.acquire_timeout.unwrap_or(DEFAULT_ACQUIRE_TIMEOUT),
            recycle_timeout: cfg.recycle_timeout.unwrap_or(DEFAULT_RECYCLE_TIMEOUT),
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_to_ten_connections() {
        let s = RedisPoolSettings::create(&RedisPoolConfig::default()).unwrap();
        assert_eq!(s.max_connections, 10);
        assert_eq!(s.connect_timeout, DEFAULT_CONNECT_TIMEOUT);
        assert_eq!(s.acquire_timeout, DEFAULT_ACQUIRE_TIMEOUT);
        assert_eq!(s.recycle_timeout, DEFAULT_RECYCLE_TIMEOUT);
    }

    #[test]
    fn max_connections_clamped_to_cap() {
        let cfg = RedisPoolConfig {
            max_connections: Some(1000),
            ..Default::default()
        };
        let s = RedisPoolSettings::create(&cfg).unwrap();
        assert_eq!(s.max_connections, MAX_CONNECTIONS_CAP);
    }

    #[test]
    fn custom_timeouts_pass_through() {
        let cfg = RedisPoolConfig {
            max_connections: Some(4),
            connect_timeout: Some(Duration::from_secs(1)),
            acquire_timeout: Some(Duration::from_millis(250)),
            recycle_timeout: Some(Duration::from_secs(2)),
        };
        let s = RedisPoolSettings::create(&cfg).unwrap();
        assert_eq!(s.max_connections, 4);
        assert_eq!(s.connect_timeout, Duration::from_secs(1));
        assert_eq!(s.acquire_timeout, Duration::from_millis(250));
        assert_eq!(s.recycle_timeout, Duration::from_secs(2));
    }

    #[test]
    fn zero_max_connections_rejected() {
        let cfg = RedisPoolConfig {
            max_connections: Some(0),
            ..Default::default()
        };
        assert_eq!(
            RedisPoolSettings::create(&cfg).unwrap_err(),
            RedisPoolConfigError::ZeroMaxConnections
        );
    }
}
