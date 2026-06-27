//! `PoolStatsReader` adapter so the redis sink factory can register the
//! pool with `air-elt-monitoring` exactly like the SQL connectors do.
//!
//! Reads deadpool's live `status()` on every scrape and splits it into
//! active (checked out) / idle (available) counts. Holds a `Pool` handle
//! (a cheap `Arc` clone); the pool is otherwise owned by the sink for the
//! process lifetime.

use deadpool_redis::Pool;

use air_elt_monitoring::{PoolConnectionCounts, PoolStatsReader};

pub struct RedisPoolStatsReader {
    pool: Pool,
}

impl RedisPoolStatsReader {
    pub(crate) fn new(pool: Pool) -> Self {
        Self { pool }
    }
}

impl PoolStatsReader for RedisPoolStatsReader {
    fn read(&self) -> PoolConnectionCounts {
        let status = self.pool.status();
        let idle = status.available as u32;
        let size = status.size as u32;
        let active = size.saturating_sub(idle);
        PoolConnectionCounts { active, idle }
    }
}
