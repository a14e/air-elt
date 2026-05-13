use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;

use crate::{MssqlSource, MssqlSourceConfig};

pub struct MssqlSourceFactory;

#[async_trait]
impl SourceFactory for MssqlSourceFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Source>, ConfigError> {
        let config = MssqlSourceConfig::try_from(cfg)?;
        let source = MssqlSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(source))
    }
}
