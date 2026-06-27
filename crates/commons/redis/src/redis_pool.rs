//! Redis-facing wrapper over [`deadpool_redis::Pool`].
//!
//! [`RedisPool`] owns the deadpool pool; [`RedisConnection`] wraps a
//! checked-out connection and runs commands / pipelines against the
//! underlying `MultiplexedConnection` (deadpool's `Connection` derefs to
//! it). deadpool owns connection lifecycle — creation, recycling
//! (health-check on checkout), and reuse — so this crate carries no
//! hand-rolled health loop, dead-connection classifier, or observer.

use deadpool_redis::{Config, Connection, Pool, Runtime};

use crate::config::{RedisPoolConfig, RedisPoolSettings};
use crate::error::RedisPoolError;
use crate::pool_stats_reader::RedisPoolStatsReader;

#[derive(Clone)]
pub struct RedisPool {
    pool: Pool,
    /// Configured pool size (deadpool `max_size`). Cached so the sink can
    /// report it from `Sink::max_connections()` without a `status()` call.
    max_connections: u32,
}

impl RedisPool {
    /// Resolve config, build the deadpool pool, then eagerly check out one
    /// connection and `PING` it — so a dead Redis (or bad auth) surfaces at
    /// construction time, not on the first batch.
    pub async fn create(url: &str, config: &RedisPoolConfig) -> Result<Self, RedisPoolError> {
        let settings = RedisPoolSettings::create(config)?;
        let mut cfg = Config::from_url(url);
        let mut pool_config = deadpool_redis::PoolConfig::new(settings.max_connections as usize);
        pool_config.timeouts = deadpool_redis::Timeouts {
            wait: Some(settings.acquire_timeout),
            create: Some(settings.connect_timeout),
            recycle: Some(settings.recycle_timeout),
        };
        cfg.pool = Some(pool_config);
        let pool = cfg
            .create_pool(Some(Runtime::Tokio1))
            .map_err(RedisPoolError::from_create)?;
        // Fail-fast probe: prove the pool can actually reach the server.
        let mut conn = pool.get().await.map_err(RedisPoolError::from_pool)?;
        redis::cmd("PING").query_async::<()>(&mut *conn).await?;
        Ok(Self {
            pool,
            max_connections: settings.max_connections,
        })
    }

    /// Check out a connection. deadpool waits up to `acquire-timeout` for a
    /// free one, recycling (health-checking) an idle connection or dialing
    /// a new one up to `max-connections`.
    pub async fn acquire(&self) -> Result<RedisConnection, RedisPoolError> {
        let conn = self.pool.get().await.map_err(RedisPoolError::from_pool)?;
        Ok(RedisConnection { conn })
    }

    /// Configured pool size. The redis sink reports this from
    /// `Sink::max_connections()` (sizing the assemble concurrency
    /// semaphore) and the factory uses it as the `max` of the
    /// `connections_open` gauge.
    pub fn max_connections(&self) -> u32 {
        self.max_connections
    }

    /// A scrape-time stats reader (holds a pool handle, reads deadpool's
    /// live `status()` for the active/idle split) for
    /// `air-elt-monitoring::register_pool_stats`. This is the single source
    /// of the `(active, idle)` derivation.
    pub fn stats_reader(&self) -> RedisPoolStatsReader {
        RedisPoolStatsReader::new(self.pool.clone())
    }
}

/// A checked-out redis connection. Drop returns it to the pool.
pub struct RedisConnection {
    conn: Connection,
}

impl RedisConnection {
    /// Run one command over the connection.
    pub async fn query<T: redis::FromRedisValue>(
        &mut self,
        cmd: &redis::Cmd,
    ) -> Result<T, RedisPoolError> {
        cmd.query_async::<T>(&mut *self.conn)
            .await
            .map_err(RedisPoolError::Redis)
    }

    /// Run a pipeline over the connection in one round-trip.
    pub async fn query_pipeline<T: redis::FromRedisValue>(
        &mut self,
        pipe: &redis::Pipeline,
    ) -> Result<T, RedisPoolError> {
        pipe.query_async::<T>(&mut *self.conn)
            .await
            .map_err(RedisPoolError::Redis)
    }
}
