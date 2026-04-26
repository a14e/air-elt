//! Shared PostgreSQL pool construction.
//!
//! All three postgres connectors (source, sink, storage) open pools through
//! `connect()`. Centralising the option builder means timeouts, the UTC
//! session TZ, and the statement-level timeout stay in sync. Everything
//! configurable is piped in via `PoolTimeouts`.

use sqlx::PgPool;
use sqlx::pool::PoolOptions;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;

pub use air_elt_commons::pool_timeouts::PoolTimeouts;

use air_elt_core::error::{RuntimeError, RuntimeResult};

/// Open a pool pre-wired with pool-level timeouts plus a session-level
/// `after_connect` that sets `TIME ZONE 'UTC'` and `statement_timeout`.
///
/// Why `SET TIME ZONE 'UTC'`: `timestamptz` always stores UTC but the wire
/// protocol renders values in the session's TZ. Forcing UTC on every new
/// connection removes a class of silent-shift bugs for operators whose
/// server default is non-UTC.
///
/// Why `SET statement_timeout`: a second line of defence on top of the
/// query timeout in the runner. Postgres kills runaway queries server-side
/// so a wedged backend cannot hold the flow forever.
pub async fn connect(url: &str, timeouts: PoolTimeouts) -> RuntimeResult<PgPool> {
    let connect_opts = PgConnectOptions::from_str(url).map_err(RuntimeError::backend)?;

    let stmt_ms = i64::try_from(timeouts.statement.as_millis()).unwrap_or(i64::MAX);
    let options: PgPoolOptions = PoolOptions::new()
        .max_connections(timeouts.max_connections)
        .min_connections(timeouts.min_connections)
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
