use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_mongodb::MongoPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{MongoStorage, MongoStorageConfig};

pub struct MongoStorageFactory;

#[async_trait]
impl StorageFactory for MongoStorageFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Storage>, ConfigError> {
        let config = MongoStorageConfig::try_from(cfg)?;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let reader = Arc::new(MongoPoolStatsReader::new());
        let storage = MongoStorage::connect(config, reader.clone())
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        monitoring.register_pool_stats(
            ComponentKind::Storage,
            &cfg.name,
            max,
            min,
            reader as Arc<dyn PoolStatsReader>,
        );
        Ok(Box::new(storage))
    }
}
