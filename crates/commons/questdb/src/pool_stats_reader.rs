//! `PoolStatsReader` wrapper for QuestDB's pg-wire pool (a `sqlx::PgPool`
//! under the hood). Cheap by-value reads of `(active, idle)` from
//! sqlx's internal counters — called once per scrape per pool by
//! `PoolStatsCollector::collect`.

use air_elt_monitoring::{PoolConnectionCounts, PoolStatsReader};
use sqlx::PgPool;

pub struct QuestDbPoolStatsReader {
    pool: PgPool,
}

impl QuestDbPoolStatsReader {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl PoolStatsReader for QuestDbPoolStatsReader {
    fn read(&self) -> PoolConnectionCounts {
        // sqlx returns `size` (total, including idle and in-use) and
        // `num_idle` (idle only). Active = size - num_idle.
        let total = self.pool.size();
        let idle = u32::try_from(self.pool.num_idle()).unwrap_or(u32::MAX);
        let active = total.saturating_sub(idle);
        PoolConnectionCounts { active, idle }
    }
}
