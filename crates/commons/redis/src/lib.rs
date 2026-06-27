//! Redis/valkey connection pool for the AIR-5 redis sink, built on the
//! standard [`deadpool_redis`] pool.
//!
//! Provides the serde pool config, the [`RedisPool`] / [`RedisConnection`]
//! wrappers (thin command/pipeline helpers over a deadpool checkout), and
//! a `PoolStatsReader` adapter that surfaces deadpool's live `status()` to
//! `air-elt-monitoring`. Connection lifecycle (creation, recycling, reuse)
//! is owned by deadpool — this crate adds no health loop or observer.

mod config;
mod error;
mod pool_stats_reader;
mod redis_pool;

pub use config::{RedisPoolConfig, RedisPoolConfigError, RedisPoolSettings};
pub use error::RedisPoolError;
pub use pool_stats_reader::RedisPoolStatsReader;
pub use redis_pool::{RedisConnection, RedisPool};
