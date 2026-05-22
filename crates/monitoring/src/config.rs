use std::time::Duration;

use air_elt_commons::interval;
use serde::{Deserialize, Serialize};

use crate::error::MonitoringError;

/// Configuration for the Prometheus metrics subsystem.
///
/// Every field carries a default so that a one-line opt-in
/// `[metrics.prometheus] enabled = true` produces a working setup. When
/// `enabled = false` (the default) no HTTP server is started and every
/// recorder is a zero-cost no-op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PrometheusConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_prefix")]
    pub prefix: String,
    #[serde(default)]
    pub summary: SummaryConfig,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SummaryConfig {
    #[serde(default = "default_window", deserialize_with = "interval::deserialize")]
    pub window: Duration,
    #[serde(
        default = "default_bucket_granularity",
        deserialize_with = "interval::deserialize"
    )]
    pub bucket_granularity: Duration,
    #[serde(default = "default_quantiles")]
    pub quantiles: Vec<f64>,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            port: default_port(),
            prefix: default_prefix(),
            summary: SummaryConfig::default(),
        }
    }
}

impl Default for SummaryConfig {
    fn default() -> Self {
        Self {
            window: default_window(),
            bucket_granularity: default_bucket_granularity(),
            quantiles: default_quantiles(),
        }
    }
}

fn default_enabled() -> bool {
    false
}
fn default_port() -> u16 {
    8080
}
fn default_prefix() -> String {
    "/metrics".to_string()
}
fn default_window() -> Duration {
    Duration::from_secs(5)
}
fn default_bucket_granularity() -> Duration {
    Duration::from_secs(1)
}
fn default_quantiles() -> Vec<f64> {
    vec![0.5, 0.9, 0.99]
}

impl PrometheusConfig {
    pub fn validate(&self) -> Result<(), MonitoringError> {
        if self.port == 0 {
            return Err(MonitoringError::InvalidConfig(
                "port must be > 0".to_string(),
            ));
        }
        if !self.prefix.starts_with('/') {
            return Err(MonitoringError::InvalidConfig(format!(
                "prefix must start with '/', got {:?}",
                self.prefix
            )));
        }
        if self.prefix.chars().any(char::is_whitespace) {
            return Err(MonitoringError::InvalidConfig(
                "prefix must not contain whitespace".to_string(),
            ));
        }
        // Axum treats `{`/`}` as path-parameter / catch-all markers. Letting
        // the operator slip them into the metrics prefix turns the endpoint
        // into a parameterised route and breaks scraping.
        if let Some(bad) = self.prefix.chars().find(|c| matches!(c, '{' | '}')) {
            return Err(MonitoringError::InvalidConfig(format!(
                "prefix must not contain {bad:?} (reserved by the HTTP router)"
            )));
        }
        self.summary.validate()?;
        Ok(())
    }
}

impl SummaryConfig {
    pub fn validate(&self) -> Result<(), MonitoringError> {
        if self.window.is_zero() {
            return Err(MonitoringError::InvalidConfig(
                "summary.window must be > 0".to_string(),
            ));
        }
        if self.bucket_granularity.is_zero() {
            return Err(MonitoringError::InvalidConfig(
                "summary.bucket-granularity must be > 0".to_string(),
            ));
        }
        if self.bucket_granularity > self.window {
            return Err(MonitoringError::InvalidConfig(
                "summary.bucket-granularity must be <= summary.window".to_string(),
            ));
        }
        if self.quantiles.is_empty() {
            return Err(MonitoringError::InvalidConfig(
                "summary.quantiles must be non-empty".to_string(),
            ));
        }
        let mut prev = 0.0_f64;
        for (idx, &q) in self.quantiles.iter().enumerate() {
            if !q.is_finite() || q <= 0.0 || q >= 1.0 {
                return Err(MonitoringError::InvalidConfig(format!(
                    "summary.quantiles[{idx}] must be in (0, 1), got {q}"
                )));
            }
            if q <= prev {
                return Err(MonitoringError::InvalidConfig(
                    "summary.quantiles must be strictly ascending".to_string(),
                ));
            }
            prev = q;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::field_reassign_with_default)]
mod tests {
    use super::*;

    #[test]
    fn defaults_pass_validation() {
        PrometheusConfig::default().validate().unwrap();
    }

    #[test]
    fn port_zero_rejected() {
        let mut cfg = PrometheusConfig::default();
        cfg.port = 0;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn prefix_must_start_with_slash() {
        let mut cfg = PrometheusConfig::default();
        cfg.prefix = "metrics".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn prefix_with_whitespace_rejected() {
        let mut cfg = PrometheusConfig::default();
        cfg.prefix = "/met rics".to_string();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn prefix_with_curly_brace_rejected() {
        for bad in ["/{tenant}", "/metrics/{id}", "/{*catchall}", "/foo}"] {
            let mut cfg = PrometheusConfig::default();
            cfg.prefix = bad.to_string();
            let err = cfg
                .validate()
                .expect_err(&format!("expected rejection of {bad:?}"));
            let msg = format!("{err}");
            assert!(
                msg.contains('{') || msg.contains('}'),
                "error message should name the offending character, got {msg:?}"
            );
        }
    }

    #[test]
    fn quantiles_must_be_in_open_unit_interval() {
        for bad in [0.0, 1.0, -0.1, 1.01, f64::NAN] {
            let mut cfg = PrometheusConfig::default();
            cfg.summary.quantiles = vec![bad];
            assert!(cfg.validate().is_err(), "expected error for {bad}");
        }
    }

    #[test]
    fn quantiles_must_be_ascending() {
        let mut cfg = PrometheusConfig::default();
        cfg.summary.quantiles = vec![0.9, 0.5];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn quantiles_must_be_non_empty() {
        let mut cfg = PrometheusConfig::default();
        cfg.summary.quantiles = vec![];
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn window_zero_rejected() {
        let mut cfg = PrometheusConfig::default();
        cfg.summary.window = Duration::ZERO;
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn bucket_granularity_must_not_exceed_window() {
        let mut cfg = PrometheusConfig::default();
        cfg.summary.bucket_granularity = Duration::from_secs(10);
        cfg.summary.window = Duration::from_secs(5);
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn enabled_true_alone_yields_valid_config() {
        let toml_src = r#"
            enabled = true
        "#;
        let cfg: PrometheusConfig = toml::from_str(toml_src).unwrap();
        assert!(cfg.enabled);
        cfg.validate().unwrap();
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.prefix, "/metrics");
        assert_eq!(cfg.summary.window, Duration::from_secs(5));
        assert_eq!(cfg.summary.bucket_granularity, Duration::from_secs(1));
        assert_eq!(cfg.summary.quantiles, vec![0.5, 0.9, 0.99]);
    }
}
