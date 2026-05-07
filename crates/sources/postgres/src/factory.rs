use async_trait::async_trait;

use air_elt_commons_pg::Dialect;
use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;
use air_elt_core::registry::SourceFactory;
use air_elt_core::traits::Source;

use crate::{PgSource, PgSourceConfig};

pub struct PgSourceFactory {
    dialect: Dialect,
}

impl PgSourceFactory {
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

impl Default for PgSourceFactory {
    fn default() -> Self {
        Self::postgres()
    }
}

#[async_trait]
impl SourceFactory for PgSourceFactory {
    async fn build(&self, cfg: &ComponentConfig) -> Result<Box<dyn Source>, ConfigError> {
        let mut config = PgSourceConfig::try_from(cfg)?;
        config.dialect = self.dialect;
        let source = PgSource::connect(cfg.name.clone(), config)
            .await
            .map_err(|e| ConfigError::Invalid {
                reason: e.to_string(),
            })?;
        Ok(Box::new(source))
    }
}
