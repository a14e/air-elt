use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;

use crate::{PgSource, PgSourceConfig};

pub struct PgSourceFactory;

#[async_trait]
impl SourceFactory for PgSourceFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Source>, ConfigError> {
        let config = PgSourceConfig::try_from(cfg)?;
        let source = PgSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(source))
    }
}
