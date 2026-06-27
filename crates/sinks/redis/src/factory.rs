//! Redis sink factory — builds the connection pool, registers its stats
//! reader with monitoring, and wraps it in a [`RedisSink`].

use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons_redis::RedisPool;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SinkFactory;
use air_elt_core::traits::Sink;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::config::RedisSinkConfig;
use crate::redis_sink::RedisSink;

pub struct RedisSinkFactory;

#[async_trait]
impl SinkFactory for RedisSinkFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Sink>, ConfigError> {
        let config = RedisSinkConfig::try_from(cfg)?;
        // Eager build: dials and PINGs one connection up front so a dead
        // redis fails at component-build time, not on the first batch.
        let pool = RedisPool::create(&config.url, &config.pool)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        // The `connections_open` gauge's ceiling is the pool size — the
        // same value `Sink::max_connections` reports to size the assemble
        // semaphore (one permit per connection). The reader reads deadpool's
        // live `status()` on every scrape for the active/idle split.
        let reader: Arc<dyn PoolStatsReader> = Arc::new(pool.stats_reader());
        monitoring.register_pool_stats(
            ComponentKind::Sink,
            &cfg.name,
            pool.max_connections(),
            0,
            reader,
        );
        Ok(Box::new(RedisSink::new(pool)))
    }
}
