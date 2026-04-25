use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;

use crate::{PgStorage, PgStorageConfig};

pub struct PgStorageFactory;

#[async_trait]
impl StorageFactory for PgStorageFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Storage>, ConfigError> {
        let config = PgStorageConfig::try_from(cfg)?;
        let storage = PgStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(storage))
    }
}
