//! Shared MySQL pool construction. Mirrors `air-elt-commons-pg::pool` —
//! same `PoolSettings` (re-exported from `air-elt-commons`), different
//! session-bootstrap SQL.

use sqlx::MySqlPool;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions};
use sqlx::pool::PoolOptions;
use std::str::FromStr;

pub use air_elt_commons::pool_settings::PoolSettings;

use air_elt_core::error::{RuntimeError, RuntimeResult};

/// Open a pool pre-wired with pool-level timeouts plus a session-level
/// `after_connect` that pins the time zone to UTC and applies a
/// statement-level execution cap.
///
/// Why `SET time_zone='+00:00'`: MySQL's `TIMESTAMP` columns are converted
/// from session-tz to UTC on write and back on read. Pinning UTC up front
/// removes silent shifts when the server default isn't UTC. `DATETIME` is
/// rejected at validation, so this only governs `TIMESTAMP`.
///
/// Why the statement-time cap: server-side cap on top of the query timeout
/// in the runner. Affects SELECTs only — DML/DDL keep running unless the
/// connection is killed (sqlx drops the conn on cancel). The variable name
/// diverges between vendors: MySQL exposes `max_execution_time` (ms,
/// integer), MariaDB exposes `max_statement_time` (seconds, decimal). We
/// probe `VERSION()` once per connection and pick the right one.
pub async fn connect(url: &str, timeouts: PoolSettings) -> RuntimeResult<MySqlPool> {
    let connect_opts = MySqlConnectOptions::from_str(url).map_err(RuntimeError::backend)?;

    let stmt_ms = u64::try_from(timeouts.statement.as_millis()).unwrap_or(u64::MAX);
    let options: MySqlPoolOptions = PoolOptions::new()
        .max_connections(timeouts.max_connections)
        .min_connections(timeouts.min_connections)
        .acquire_timeout(timeouts.acquire)
        .idle_timeout(Some(timeouts.idle))
        .max_lifetime(Some(timeouts.max_lifetime))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                sqlx::query("SET SESSION time_zone = '+00:00'")
                    .execute(&mut *conn)
                    .await?;
                let (version,): (String,) = sqlx::query_as("SELECT VERSION()")
                    .fetch_one(&mut *conn)
                    .await?;
                let stmt = if version.to_ascii_lowercase().contains("mariadb") {
                    let secs = stmt_ms as f64 / 1000.0;
                    format!("SET SESSION max_statement_time = {secs}")
                } else {
                    format!("SET SESSION max_execution_time = {stmt_ms}")
                };
                sqlx::query(&stmt).execute(&mut *conn).await?;
                Ok(())
            })
        });

    let connect_timeout = timeouts.connect;
    let fut = options.connect_with(connect_opts);
    let pool = tokio::time::timeout(connect_timeout, fut)
        .await
        .map_err(|_| {
            RuntimeError::Other(format!(
                "mysql connect timed out after {:?}",
                connect_timeout
            ))
        })?
        .map_err(RuntimeError::backend)?;
    Ok(pool)
}
