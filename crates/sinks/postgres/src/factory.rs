use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_pg::Dialect;
use air_elt_commons_pg::PgPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SinkFactory;
use air_elt_core::traits::Sink;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{PgSink, PgSinkConfig};

pub struct PgSinkFactory {
    dialect: Dialect,
}

impl PgSinkFactory {
    /// Factory bound to the standard PostgreSQL dialect (`type = "postgres"`).
    pub fn postgres() -> Self {
        Self {
            dialect: Dialect::Postgres,
        }
    }

    /// Factory bound to the CockroachDB dialect (`type = "cockroachdb"`).
    pub fn cockroach() -> Self {
        Self {
            dialect: Dialect::Cockroach,
        }
    }
}

impl Default for PgSinkFactory {
    fn default() -> Self {
        Self::postgres()
    }
}

#[async_trait]
impl SinkFactory for PgSinkFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Sink>, ConfigError> {
        let mut config = PgSinkConfig::try_from(cfg)?;
        config.dialect = self.dialect;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let sink = PgSink::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        let reader: Arc<dyn PoolStatsReader> = Arc::new(PgPoolStatsReader::new(sink.pool()));
        monitoring.register_pool_stats(ComponentKind::Sink, &cfg.name, max, min, reader);
        Ok(Box::new(sink))
    }
}
