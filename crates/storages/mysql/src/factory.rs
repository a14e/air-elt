use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_mysql::MySqlPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{MySqlStorage, MySqlStorageConfig};

pub struct MySqlStorageFactory;

#[async_trait]
impl StorageFactory for MySqlStorageFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Storage>, ConfigError> {
        let config = MySqlStorageConfig::try_from(cfg)?;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let storage = MySqlStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        let reader: Arc<dyn PoolStatsReader> = Arc::new(MySqlPoolStatsReader::new(storage.pool()));
        monitoring.register_pool_stats(ComponentKind::Storage, &cfg.name, max, min, reader);
        Ok(Box::new(storage))
    }
}
