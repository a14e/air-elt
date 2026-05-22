use thiserror::Error;

#[derive(Debug, Error)]
pub enum MonitoringError {
    #[error("invalid metrics config: {0}")]
    InvalidConfig(String),

    #[error("prometheus registry error: {0}")]
    Registry(#[from] prometheus::Error),

    #[error("metrics server io error: {0}")]
    ServerIo(#[from] std::io::Error),
}
