use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_mongodb::MongoPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SinkFactory;
use air_elt_core::traits::Sink;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{MongoSink, MongoSinkConfig};

pub struct MongoSinkFactory;

#[async_trait]
impl SinkFactory for MongoSinkFactory {
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Sink>, ConfigError> {
        let config = MongoSinkConfig::try_from(cfg)?;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let reader = Arc::new(MongoPoolStatsReader::new());
        let sink = MongoSink::connect(config, reader.clone())
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        monitoring.register_pool_stats(
            ComponentKind::Sink,
            &cfg.name,
            max,
            min,
            reader as Arc<dyn PoolStatsReader>,
        );
        Ok(Box::new(sink))
    }
}
