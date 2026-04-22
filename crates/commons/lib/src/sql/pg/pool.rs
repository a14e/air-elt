//! Shared PostgreSQL pool construction.
//!
//! All three postgres connectors (source, sink, storage) open pools through
//! `connect()`. Centralising the option builder means timeouts, the UTC
//! session TZ, and the statement-level timeout stay in sync. Everything
//! configurable is piped in via `PoolTimeouts`.

use std::time::Duration;

use sqlx::PgPool;
use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

use air_elt_core::error::{RuntimeError, RuntimeResult};

#[derive(Debug, Clone, Copy)]
pub struct PoolTimeouts {
    pub connect: Duration,
    pub acquire: Duration,
    pub idle: Duration,
    pub max_lifetime: Duration,
    pub statement: Duration,
}

impl PoolTimeouts {
    /// Conservative defaults. Operators can override any of them per connector.
    ///
    /// - connect=5s: distinguishes "unreachable DB" from "slow DB" fast;
    /// - acquire=10s: with `max_connections=5` nobody should wait that long;
    /// - idle=300s: keep the pool warm but don't hoard connections;
    /// - max_lifetime=1800s: rotate through a load balancer cleanly;
    /// - statement=30s: default balance for OLTP-shaped ELT batches. Long
    ///   jobs override via `statement_timeout_secs` in the connector config.
    pub const fn defaults() -> Self {
        Self {
            connect: Duration::from_secs(5),
            acquire: Duration::from_secs(10),
            idle: Duration::from_secs(300),
            max_lifetime: Duration::from_secs(1800),
            statement: Duration::from_secs(30),
        }
    }

    pub fn from_options(
        connect: Option<u64>,
        acquire: Option<u64>,
        idle: Option<u64>,
        max_lifetime: Option<u64>,
        statement: Option<u64>,
    ) -> Self {
        let defaults = Self::defaults();
        Self {
            connect: connect.map(Duration::from_secs).unwrap_or(defaults.connect),
            acquire: acquire.map(Duration::from_secs).unwrap_or(defaults.acquire),
            idle: idle.map(Duration::from_secs).unwrap_or(defaults.idle),
            max_lifetime: max_lifetime
                .map(Duration::from_secs)
                .unwrap_or(defaults.max_lifetime),
            statement: statement
                .map(Duration::from_secs)
                .unwrap_or(defaults.statement),
        }
    }
}

/// Open a pool pre-wired with pool-level timeouts plus a session-level
/// `after_connect` that sets `TIME ZONE 'UTC'` and `statement_timeout`.
///
/// Why `SET TIME ZONE 'UTC'`: `timestamptz` always stores UTC but the wire
/// protocol renders values in the session's TZ. Forcing UTC on every new
/// connection removes a class of silent-shift bugs for operators whose
/// server default is non-UTC.
///
/// Why `SET statement_timeout`: a second line of defence on top of the
/// operation timeout in the runner. Postgres kills runaway queries server-side
/// so a wedged backend cannot hold the flow forever.
pub async fn connect(url: &str, timeouts: PoolTimeouts) -> RuntimeResult<PgPool> {
    let connect_opts = PgConnectOptions::from_str(url).map_err(RuntimeError::backend)?;

    let stmt_ms = timeouts.statement.as_millis() as i64;
    let options: PgPoolOptions = PoolOptions::new()
        .max_connections(5)
        .acquire_timeout(timeouts.acquire)
        .idle_timeout(Some(timeouts.idle))
        .max_lifetime(Some(timeouts.max_lifetime))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET TIME ZONE 'UTC'")
                    .execute(&mut *conn)
                    .await?;
                let stmt = format!("SET statement_timeout = {stmt_ms}");
                sqlx::query(&stmt).execute(&mut *conn).await?;
                Ok(())
            })
        });

    let connect_timeout = timeouts.connect;

    // Wrap the whole `.connect_with(opts)` call in a tokio timeout so even a
    // misconfigured `PgConnectOptions` can't block longer than `connect`.
    let fut = options.connect_with(connect_opts);
    let pool = tokio::time::timeout(connect_timeout, fut)
        .await
        .map_err(|_| {
            RuntimeError::Other(format!(
                "postgres connect timed out after {:?}",
                connect_timeout
            ))
        })?
        .map_err(RuntimeError::backend)?;
    Ok(pool)
}
