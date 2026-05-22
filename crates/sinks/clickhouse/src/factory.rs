use async_trait::async_trait;

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SinkFactory;
use air_elt_core::traits::Sink;
use air_elt_monitoring::MonitoringManager;

use crate::{ChSink, ChSinkConfig};

pub struct ChSinkFactory;

#[async_trait]
impl SinkFactory for ChSinkFactory {
    /// ClickHouse uses a reqwest HTTP client, not a database connection
    /// pool, so it has no pool stats to register. The `monitoring` arg
    /// is accepted to satisfy the trait but intentionally unused.
    async fn build(
        &self,
        cfg: &ComponentConfig,
        _monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Sink>, ConfigError> {
        let config = ChSinkConfig::try_from(cfg)?;
        let sink = ChSink::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(sink))
    }
}
