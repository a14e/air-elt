use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_mongodb::MongoPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::config::validation::SamplingConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{MongoSource, MongoSourceConfig};

pub struct MongoSourceFactory;

#[async_trait]
impl SourceFactory for MongoSourceFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Source>, ConfigError> {
        let config = MongoSourceConfig::try_from(cfg)?;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        // Build the reader up front so the driver's CMAP closure can
        // capture it inside `connect()`. The factory hands the same Arc
        // to monitoring; the collector holds it strong for the process
        // lifetime, so connectors carry no pool-stats fields themselves.
        let reader = Arc::new(MongoPoolStatsReader::new());
        let source = MongoSource::connect(cfg.name.clone(), config, reader.clone())
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        monitoring.register_pool_stats(
            ComponentKind::Source,
            &cfg.name,
            max,
            min,
            reader as Arc<dyn PoolStatsReader>,
        );
        Ok(Box::new(source))
    }

    fn sampling_default(&self) -> SamplingConfig {
        SamplingConfig::enabled_default()
    }
}
