use async_trait::async_trait;

use air_elt_commons_pg::Dialect;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::StorageFactory;
use air_elt_core::traits::Storage;

use crate::{PgStorage, PgStorageConfig};

pub struct PgStorageFactory {
    dialect: Dialect,
}

impl PgStorageFactory {
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

impl Default for PgStorageFactory {
    fn default() -> Self {
        Self::postgres()
    }
}

#[async_trait]
impl StorageFactory for PgStorageFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Storage>, ConfigError> {
        let mut config = PgStorageConfig::try_from(cfg)?;
        config.dialect = self.dialect;
        let storage = PgStorage::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(storage))
    }
}
