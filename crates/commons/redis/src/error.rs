//! Error type for the redis pool.
//!
//! The sink crate (AIR-5 Stage 4) maps these into `core::RuntimeError` at
//! its boundary; this crate stays free of a `core` dependency.

use deadpool_redis::{CreatePoolError, PoolError};
use thiserror::Error;

use crate::config::RedisPoolConfigError;

#[derive(Debug, Error)]
pub enum RedisPoolError {
    /// A redis-rs driver error (dial, command execution, protocol).
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),

    /// Building the connection pool failed (bad URL or pool config).
    #[error("redis pool build failed: {0}")]
    Build(String),

    /// No connection became free within `acquire-timeout` (pool saturated)
    /// or recycle/create timed out.
    #[error("redis pool acquire timed out")]
    AcquireTimeout,

    /// The pool is otherwise unable to hand out a connection (closed, no
    /// runtime, post-create hook failure).
    #[error("redis pool unavailable: {0}")]
    Unavailable(String),

    /// Invalid pool configuration (rejected before any I/O).
    #[error(transparent)]
    Config(#[from] RedisPoolConfigError),
}

impl RedisPoolError {
    /// Map a pool build failure into the redis-specific surface.
    pub(crate) fn from_create(error: CreatePoolError) -> Self {
        RedisPoolError::Build(error.to_string())
    }

    /// Map a `pool.get()` failure. A backend error carries the real
    /// `RedisError` (feeding the same surface as a command error); a
    /// wait/create timeout collapses to `AcquireTimeout` (a recycle
    /// timeout never surfaces here — deadpool retries it internally,
    /// discarding the stale connection); anything else is `Unavailable`.
    pub(crate) fn from_pool(error: PoolError) -> Self {
        match error {
            PoolError::Backend(err) => RedisPoolError::Redis(err),
            PoolError::Timeout(_) => RedisPoolError::AcquireTimeout,
            other => RedisPoolError::Unavailable(other.to_string()),
        }
    }
}
