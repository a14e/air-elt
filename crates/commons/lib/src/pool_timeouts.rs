//! Provider-agnostic pool tunables. Both pg and mysql pools accept the same
//! struct — only the `after_connect` session SQL differs per dialect.

use std::time::Duration;

#[derive(Debug, Clone, Copy)]
pub struct PoolTimeouts {
    pub connect: Duration,
    pub acquire: Duration,
    pub idle: Duration,
    pub max_lifetime: Duration,
    pub statement: Duration,
    pub max_connections: u32,
    pub min_connections: u32,
}

impl PoolTimeouts {
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

    pub fn from_options(
        connect: Option<Duration>,
        acquire: Option<Duration>,
        idle: Option<Duration>,
        max_lifetime: Option<Duration>,
        statement: Option<Duration>,
        max_connections: Option<u32>,
        min_connections: Option<u32>,
    ) -> Self {
        let defaults = Self::defaults();
        Self {
            connect: connect.unwrap_or(defaults.connect),
            acquire: acquire.unwrap_or(defaults.acquire),
            idle: idle.unwrap_or(defaults.idle),
            max_lifetime: max_lifetime.unwrap_or(defaults.max_lifetime),
            statement: statement.unwrap_or(defaults.statement),
            max_connections: max_connections.unwrap_or(defaults.max_connections).min(100),
            min_connections: min_connections.unwrap_or(0),
        }
    }
}
