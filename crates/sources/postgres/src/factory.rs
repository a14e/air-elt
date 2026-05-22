use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_pg::Dialect;
use air_elt_commons_pg::PgPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{PgSource, PgSourceConfig};

pub struct PgSourceFactory {
    dialect: Dialect,
}

impl PgSourceFactory {
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

impl Default for PgSourceFactory {
    fn default() -> Self {
        Self::postgres()
    }
}

#[async_trait]
impl SourceFactory for PgSourceFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Source>, ConfigError> {
        let mut config = PgSourceConfig::try_from(cfg)?;
        config.dialect = self.dialect;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let source = PgSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        let reader: Arc<dyn PoolStatsReader> = Arc::new(PgPoolStatsReader::new(source.pool()));
        monitoring.register_pool_stats(ComponentKind::Source, &cfg.name, max, min, reader);
        Ok(Box::new(source))
    }
}
