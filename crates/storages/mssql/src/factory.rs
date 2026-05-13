use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;

use crate::{MssqlStorage, MssqlStorageConfig};

pub struct MssqlStorageFactory;

#[async_trait]
impl StorageFactory for MssqlStorageFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Storage>, ConfigError> {
        let config = MssqlStorageConfig::try_from(cfg)?;
        let storage = MssqlStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(storage))
    }
}
