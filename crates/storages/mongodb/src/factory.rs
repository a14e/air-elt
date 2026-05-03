use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;

use crate::{MongoStorage, MongoStorageConfig};

pub struct MongoStorageFactory;

#[async_trait]
impl StorageFactory for MongoStorageFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Storage>, ConfigError> {
        let config = MongoStorageConfig::try_from(cfg)?;
        let storage = MongoStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(storage))
    }
}
