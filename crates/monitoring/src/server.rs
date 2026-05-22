use std::net::{Ipv4Addr, SocketAddr};

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use prometheus::{Encoder, TextEncoder};
use tokio::sync::watch;

use crate::config::PrometheusConfig;
use crate::error::MonitoringError;
use crate::manager::MetricsScraper;

/// Run the metrics HTTP server. The future resolves when `shutdown`
/// flips to `true`. Returns early without binding when the scraper is
/// disabled — callers may simply skip the spawn in that case, but
/// guarding both ends keeps the caller side dumb.
pub async fn serve(
    scraper: MetricsScraper,
    shutdown: watch::Receiver<bool>,
) -> Result<(), MonitoringError> {
    let cfg = match scraper.config() {
        Some(c) => c.clone(),
        None => {
            tracing::debug!("metrics server skipped: monitoring disabled");
            return Ok(());
        }
    };
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, cfg.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        port = cfg.port,
        prefix = %cfg.prefix,
        "metrics server listening"
    );
    serve_on_listener(scraper, listener, shutdown).await
}

/// Serve metrics on a pre-bound `TcpListener`. Used both internally by
/// [`serve`] and externally by integration tests that need to bind on
/// an ephemeral port (port 0) to avoid collisions across parallel test
/// runs.
pub async fn serve_on_listener(
    scraper: MetricsScraper,
    listener: tokio::net::TcpListener,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), MonitoringError> {
    let cfg = match scraper.config() {
        Some(c) => c.clone(),
        None => return Ok(()),
    };
    let app = build_router(scraper, cfg);
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = shutdown.changed().await;
        })
        .await?;
    Ok(())
}

fn build_router(scraper: MetricsScraper, cfg: PrometheusConfig) -> Router {
    // Deliberately no `ConcurrencyLimitLayer` here. The metrics
    // endpoint is internal-only, scrape cadence is on the order of
    // tens of seconds, and the underlying `Registry::gather` is fast
    // under our cardinality. A parallel-scrape DoS amplification
    // would require an attacker already inside the network — at
    // which point a busy /metrics endpoint is not the threat model.
    // Re-evaluate only if we ever expose this externally.
    Router::new()
        .route(&cfg.prefix, get(serve_metrics))
        .fallback(serve_not_found)
        .with_state(scraper)
}

async fn serve_metrics(State(scraper): State<MetricsScraper>) -> impl IntoResponse {
    let families = scraper.gather();
    let mut buffer = Vec::with_capacity(4096);
    let encoder = TextEncoder::new();
    if let Err(e) = encoder.encode(&families, &mut buffer) {
        tracing::error!(error = %e, "failed to encode metrics");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            format!("encode error: {e}").into_bytes(),
        );
    }
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        buffer,
    )
}

async fn serve_not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, "not found")
}
