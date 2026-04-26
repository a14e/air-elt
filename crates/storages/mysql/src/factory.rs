use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;

use crate::{MySqlStorage, MySqlStorageConfig};

pub struct MySqlStorageFactory;

#[async_trait]
impl StorageFactory for MySqlStorageFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Storage>, ConfigError> {
        let config = MySqlStorageConfig::try_from(cfg)?;
        let storage = MySqlStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(storage))
    }
}
