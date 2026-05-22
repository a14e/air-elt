//! Provider-agnostic pool tunables. Both pg and mysql pools accept the same
//! struct — only the `after_connect` session SQL differs per dialect.

use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PoolSettingsError {
    /// `max-connections = 0` would translate to a `Semaphore::new(0)`
    /// in the runtime concurrency manager, and every flow referencing
    /// that component would wait for a permit forever. Reject at
    /// connector-build time so the operator sees a config error
    /// rather than a hung process.
    #[error("max-connections must be ≥ 1 (got 0)")]
    ZeroMaxConnections,
}

#[derive(Debug, Clone, Copy)]
pub struct PoolSettings {
    pub connect: Duration,
    pub acquire: Duration,
    pub idle: Duration,
    pub max_lifetime: Duration,
    pub statement: Duration,
    pub max_connections: u32,
    pub min_connections: u32,
}

impl PoolSettings {
    /// Conservative defaults. Operators can override any of them per connector.
    ///
    /// - connect=5s: distinguishes "unreachable DB" from "slow DB" fast;
    /// - acquire=10s: with `max_connections=5` nobody should wait that long;
    /// - idle=300s: keep the pool warm but don't hoard connections;
    /// - max_lifetime=1800s: rotate through a load balancer cleanly;
    /// - statement=30s: default for OLTP-shaped ELT batches. Long jobs
    ///   override via `statement_timeout_secs` in the connector config.
    pub const fn defaults() -> Self {
        Self {
            connect: Duration::from_secs(5),
            acquire: Duration::from_secs(10),
            idle: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(1800),
            statement: Duration::from_secs(30),
            max_connections: 5,
            min_connections: 0,
        }
    }

    /// Resolve just the `(max_connections, min_connections)` pair from
    /// raw config options, using the same defaults and clamps as
    /// [`Self::from_options`]. Callers that need the bounds before the
    /// rest of the pool settings exist (factory passing them to
    /// `MonitoringManager::register_pool_stats` before opening the pool)
    /// use this to avoid building the full struct twice.
    ///
    /// `min` is clamped to `max` so the published
    /// `air_elt_pool_connections_min` gauge cannot exceed
    /// `air_elt_pool_connections_max`. sqlx already clamps internally,
    /// but without this we'd surface the operator-declared value to the
    /// metrics endpoint and mislead dashboards.
    pub fn resolve_bounds(
        max_connections: Option<u32>,
        min_connections: Option<u32>,
    ) -> Result<(u32, u32), PoolSettingsError> {
        let defaults = Self::defaults();
        let max = max_connections.unwrap_or(defaults.max_connections).min(100);
        if max == 0 {
            return Err(PoolSettingsError::ZeroMaxConnections);
        }
        let min = min_connections.unwrap_or(0).min(max);
        Ok((max, min))
    }

    pub fn from_options(
        connect: Option<Duration>,
        acquire: Option<Duration>,
        idle: Option<Duration>,
        max_lifetime: Option<Duration>,
        statement: Option<Duration>,
        max_connections: Option<u32>,
        min_connections: Option<u32>,
    ) -> Result<Self, PoolSettingsError> {
        let defaults = Self::defaults();
        // Single source of truth for the bound extraction.
        let (max_connections, min_connections) =
            Self::resolve_bounds(max_connections, min_connections)?;
        Ok(Self {
            connect: connect.unwrap_or(defaults.connect),
            acquire: acquire.unwrap_or(defaults.acquire),
            idle: idle.unwrap_or(defaults.idle),
            max_lifetime: max_lifetime.unwrap_or(defaults.max_lifetime),
            statement: statement.unwrap_or(defaults.statement),
            max_connections,
            min_connections,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_through() {
        let (max, min) = PoolSettings::resolve_bounds(None, None).unwrap();
        assert_eq!(max, 5);
        assert_eq!(min, 0);
    }

    #[test]
    fn max_clamped_to_100() {
        let (max, min) = PoolSettings::resolve_bounds(Some(500), Some(0)).unwrap();
        assert_eq!(max, 100);
        assert_eq!(min, 0);
    }

    #[test]
    fn min_clamped_to_max_on_resolve_bounds() {
        let (max, min) = PoolSettings::resolve_bounds(Some(5), Some(200)).unwrap();
        assert_eq!(max, 5);
        assert_eq!(
            min, 5,
            "min must clamp to max so the published gauge is consistent"
        );
    }

    #[test]
    fn min_clamped_to_max_on_from_options() {
        let settings =
            PoolSettings::from_options(None, None, None, None, None, Some(5), Some(200)).unwrap();
        assert_eq!(settings.max_connections, 5);
        assert_eq!(
            settings.min_connections, 5,
            "from_options must clamp via resolve_bounds"
        );
    }

    #[test]
    fn zero_max_rejected() {
        let err = PoolSettings::resolve_bounds(Some(0), None).unwrap_err();
        assert!(matches!(err, PoolSettingsError::ZeroMaxConnections));
    }
}
