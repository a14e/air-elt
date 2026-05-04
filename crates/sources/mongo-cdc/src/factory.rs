use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::config::validation::SamplingConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;

use crate::{MongoCdcSource, MongoCdcSourceConfig};

pub struct MongoCdcSourceFactory;

#[async_trait]
impl SourceFactory for MongoCdcSourceFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Source>, ConfigError> {
        let config = MongoCdcSourceConfig::try_from(cfg)?;
        let source = MongoCdcSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(source))
    }

    fn sampling_default(&self) -> SamplingConfig {
        SamplingConfig::enabled_default()
    }
}
