use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SinkFactory;
use air_elt_core::traits::Sink;

use crate::{MssqlSink, MssqlSinkConfig};

pub struct MssqlSinkFactory;

#[async_trait]
impl SinkFactory for MssqlSinkFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Sink>, ConfigError> {
        let config = MssqlSinkConfig::try_from(cfg)?;
        let sink = MssqlSink::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(sink))
    }
}
