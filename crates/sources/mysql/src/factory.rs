use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_mysql::MySqlPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{MySqlSource, MySqlSourceConfig};

pub struct MySqlSourceFactory;

#[async_trait]
impl SourceFactory for MySqlSourceFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Source>, ConfigError> {
        let config = MySqlSourceConfig::try_from(cfg)?;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let source = MySqlSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        let reader: Arc<dyn PoolStatsReader> = Arc::new(MySqlPoolStatsReader::new(source.pool()));
        monitoring.register_pool_stats(ComponentKind::Source, &cfg.name, max, min, reader);
        Ok(Box::new(source))
    }
}
