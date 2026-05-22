use std::sync::Arc;

use async_trait::async_trait;

use air_elt_commons::pool_settings::PoolSettings;
use air_elt_commons_questdb::QuestDbPoolStatsReader;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SinkFactory;
use air_elt_core::traits::Sink;
use air_elt_monitoring::{ComponentKind, MonitoringManager, PoolStatsReader};

use crate::{QuestDbSink, QuestDbSinkConfig};

pub struct QuestDbSinkFactory;

#[async_trait]
impl SinkFactory for QuestDbSinkFactory {
    /// QuestDB drives a `sqlx::PgPool` over the server's pg-wire surface;
    /// the reader wires the live counts the same way as the
    /// postgres/mysql connectors. ILP (the alternative transport) is
    /// not used by this sink.
    async fn build(
        &self,
        cfg: &ComponentConfig,
        monitoring: &mut MonitoringManager,
    ) -> Result<Box<dyn Sink>, ConfigError> {
        let config = QuestDbSinkConfig::try_from(cfg)?;
        let (max, min) =
            PoolSettings::resolve_bounds(config.max_connections, config.min_connections).map_err(
                |e| ConfigError::Invalid {
                    reason: e.to_string(),
                },
            )?;
        let sink = QuestDbSink::connect(config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        let reader: Arc<dyn PoolStatsReader> = Arc::new(QuestDbPoolStatsReader::new(sink.pool()));
        monitoring.register_pool_stats(ComponentKind::Sink, &cfg.name, max, min, reader);
        Ok(Box::new(sink))
    }
}
