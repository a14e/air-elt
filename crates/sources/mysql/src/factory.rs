use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;

use crate::{MySqlSource, MySqlSourceConfig};

pub struct MySqlSourceFactory;

#[async_trait]
impl SourceFactory for MySqlSourceFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Source>, ConfigError> {
        let config = MySqlSourceConfig::try_from(cfg)?;
        let source = MySqlSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(source))
    }
}
