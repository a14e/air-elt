//! pg-wire pool helpers for QuestDB.
//!
//! QuestDB exposes a Postgres-wire control surface that we drive via a
//! plain `sqlx::PgPool` (we cannot depend on `air-elt-commons-pg` per
//! project rule — see this crate's `lib.rs` docs). The pool handles
//! schema introspection and INSERT writes.
//!
//! There is no wrapper struct: callers hold the `PgPool` directly and
//! call the free functions in this module.
//!
//! Pool stats are NOT wired here. The driver-pool instant gauges
//! are driven by a snapshot-at-scrape source registered with the
//! monitoring collector — see
//! `pool_stats_reader::QuestDbPoolStatsReader`. The factory wraps the
//! returned `PgPool`, registers the stats reader, and the collector
//! polls it on every scrape.

use std::str::FromStr;

use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tracing::debug;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_core::error::{RuntimeError, RuntimeResult};

/// Open a pg-wire pool against the QuestDB server. The pool settings
/// honour every field of [`PoolSettings`]. Every connection is opened
/// with `SET statement_timeout` derived from `pool.statement` so a
/// stalled QuestDB query (WAL apply, write-lock contention, etc.) is
/// cancelled server-side rather than blocking the test pool for the
/// full TCP / `tokio::time::timeout` window.
pub async fn connect_pool(pg_url: &str, pool: PoolSettings) -> RuntimeResult<PgPool> {
    let opts = PgConnectOptions::from_str(pg_url).map_err(RuntimeError::backend)?;
    // Mirror `air_elt_commons_pg::pool` — saturate to `i64::MAX` on
    // overflow rather than silently truncating with `as u64`. QuestDB's
    // pg wire driver accepts either signed or unsigned in
    // `SET statement_timeout`; pg's idiom carries over cleanly.
    let statement_timeout_ms = i64::try_from(pool.statement.as_millis()).unwrap_or(i64::MAX);
    let pool_opts: PgPoolOptions = PgPoolOptions::new()
        .max_connections(pool.max_connections)
        .min_connections(pool.min_connections)
        .acquire_timeout(pool.acquire)
        .idle_timeout(Some(pool.idle))
        .max_lifetime(Some(pool.max_lifetime))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = {statement_timeout_ms}"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        });

    let connect_fut = pool_opts.connect_with(opts);
    let pg_pool = tokio::time::timeout(pool.connect, connect_fut)
        .await
        .map_err(|_| {
            RuntimeError::Other(format!(
                "questdb pg-wire connect timed out after {:?}",
                pool.connect
            ))
        })?
        .map_err(RuntimeError::backend)?;
    debug!("questdb pg-wire pool opened");
    Ok(pg_pool)
}

/// `SELECT 1` — server-side liveness probe.
pub async fn ping(pool: &PgPool) -> RuntimeResult<()> {
    sqlx::query("SELECT 1")
        .execute(pool)
        .await
        .map_err(RuntimeError::backend)?;
    Ok(())
}
