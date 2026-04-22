use serde::{Deserialize, Serialize};

use air_elt_core::config::model::ComponentConfig;
use air_elt_core::error::ConfigError;

/// Storage connection config.
///
/// Why no `schema` field: Postgres schema/search_path interaction with
/// `$user`, default schema, and role privileges is noisy enough that MVP
/// punts on it. If an operator needs the cursor table in a non-default
/// schema, they encode it in the URL via
/// `?options=-c%20search_path%3Danalytics`. libpq applies this on every new
/// pool connection, so the embedded `sqlx::migrate!` also lands in the
/// chosen schema automatically.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct PgStorageConfig {
    pub url: String,
    #[serde(default)]
    pub connect_timeout_secs: Option<u64>,
    #[serde(default)]
    pub acquire_timeout_secs: Option<u64>,
    #[serde(default)]
    pub idle_timeout_secs: Option<u64>,
    #[serde(default)]
    pub max_lifetime_secs: Option<u64>,
    #[serde(default)]
    pub statement_timeout_secs: Option<u64>,
}

// Why: dedicated TryFrom per connector keeps config parsing co-located with
// the config struct. A commons helper would need a generic + trait bounds
// dance that's no cleaner than three tiny impls across three connector crates
// that already depend on commons.
impl TryFrom<&ComponentConfig> for PgStorageConfig {
    type Error = ConfigError;

    fn try_from(cfg: &ComponentConfig) -> Result<Self, Self::Error> {
        cfg.config
            .clone()
            .try_into::<Self>()
            .map_err(|source| ConfigError::TomlParse {
                path: std::path::PathBuf::from(format!("<inline:{}>", cfg.name)),
                source,
            })
    }
}
