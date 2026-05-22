use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use air_elt_monitoring::{
    ComponentKind, ErrorStage, FlowLabels, MetricsScraper, MonitoringManager, PoolConnectionCounts,
    PoolStatsReader, PrometheusConfig, RowOp,
};
use tokio::net::TcpListener;
use tokio::sync::watch;

struct FakeReader {
    active: AtomicU32,
    idle: AtomicU32,
}

impl FakeReader {
    fn new(active: u32, idle: u32) -> Arc<Self> {
        Arc::new(Self {
            active: AtomicU32::new(active),
            idle: AtomicU32::new(idle),
        })
    }
}

impl PoolStatsReader for FakeReader {
    fn read(&self) -> PoolConnectionCounts {
        PoolConnectionCounts {
            active: self.active.load(Ordering::Relaxed),
            idle: self.idle.load(Ordering::Relaxed),
        }
    }
}

fn enabled_config() -> PrometheusConfig {
    PrometheusConfig {
        enabled: true,
        ..PrometheusConfig::default()
    }
}

fn sample_labels() -> FlowLabels {
    FlowLabels {
        flow: "orders".to_string(),
        source_name: "pg_src".to_string(),
        source_kind: "postgres".to_string(),
        sink_name: "ch_sink".to_string(),
        sink_kind: "clickhouse".to_string(),
        storage_name: "pg_state".to_string(),
        storage_kind: "postgres".to_string(),
    }
}

struct ServerHandle {
    base_url: String,
    shutdown_tx: watch::Sender<bool>,
    join: tokio::task::JoinHandle<()>,
}

impl ServerHandle {
    async fn spawn(scraper: MetricsScraper) -> Self {
        let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .await
            .expect("bind ephemeral port");
        let local_addr = listener.local_addr().expect("local addr");
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let join = tokio::spawn(async move {
            air_elt_monitoring::server::serve_on_listener(scraper, listener, shutdown_rx)
                .await
                .expect("server task");
        });
        Self {
            base_url: format!("http://{local_addr}"),
            shutdown_tx,
            join,
        }
    }

    async fn stop(self) {
        let _ = self.shutdown_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_secs(5), self.join).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn metrics_endpoint_exposes_expected_names() {
    let mut manager = MonitoringManager::new(enabled_config()).expect("build manager");

    let flow = manager.flow_recorder(sample_labels());
    flow.inc_rows_read(7, RowOp::Upsert);
    flow.inc_rows_written(3, RowOp::Delete);
    flow.inc_rows_skipped(2, RowOp::Delete);
    flow.inc_error(ErrorStage::Sink, "backend");
    drop(flow.start_recording_fetch());
    drop(flow.start_recording_transform());
    drop(flow.start_recording_sink());

    let pool = manager.lock_recorder(ComponentKind::Sink, "ch_sink");
    manager.set_lock_max(ComponentKind::Sink, "ch_sink", 8);
    // Drive both queue and active TIGs once each so skip-zero doesn't
    // hide the families from `/metrics` — the assertions below check
    // `# TYPE` presence, which only appears once a slot has been
    // touched.
    drop(pool.queue_guard());
    drop(pool.active_guard());
    manager.register_pool_stats(
        ComponentKind::Sink,
        "ch_sink",
        5,
        0,
        FakeReader::new(1, 1) as Arc<dyn PoolStatsReader>,
    );
    manager.set_counts(1, 1, 1, 1);

    let scraper = manager.into_scraper();
    let server = ServerHandle::spawn(scraper).await;
    let client = reqwest::Client::new();
    let metrics_url = format!("{}/metrics", server.base_url);

    let response = client.get(&metrics_url).send().await.expect("GET /metrics");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        content_type.starts_with("text/plain"),
        "unexpected content-type: {content_type}"
    );
    let body = response.text().await.expect("body");

    let expected_names = [
        "air_elt_fetch_seconds",
        "air_elt_transform_seconds",
        "air_elt_sink_seconds",
        "air_elt_rows_total",
        "air_elt_errors_total",
        "air_elt_lock_max",
        "air_elt_lock_queue_seconds_integral",
        "air_elt_lock_active_seconds_integral",
        "air_elt_pool_connections_active",
        "air_elt_pool_connections_idle",
        "air_elt_pool_connections_max",
        "air_elt_pool_connections_min",
        "flows",
        "sources",
        "sinks",
        "storages",
        "process_cpu_seconds_total",
        "process_resident_memory_bytes",
        "process_start_time_seconds",
        "memory_used_bytes_seconds_integral",
        "memory_available_bytes_seconds_integral",
        "memory_free_bytes_seconds_integral",
        "memory_total_bytes",
        "cpu_count",
    ];
    for name in expected_names {
        assert!(
            body.contains(&format!("# TYPE {name} ")) || body.contains(&format!("# HELP {name} ")),
            "metric {name} not exposed in body:\n{body}"
        );
    }

    // Prometheus renders labels alphabetically; we don't assume order,
    // just that the family name and the flow label co-occur.
    assert!(
        body.lines().any(|line| {
            line.starts_with("air_elt_rows_total{") && line.contains("flow=\"orders\"")
        }),
        "rows_total{{flow=orders}} not found in body:\n{body}"
    );
    assert!(
        body.contains("stage=\"read\""),
        "stage=read label not found in body:\n{body}"
    );
    assert!(
        body.contains("op=\"upsert\""),
        "op=upsert label not found in body:\n{body}"
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_path_returns_404() {
    let manager = MonitoringManager::new(enabled_config()).expect("build manager");
    let scraper = manager.into_scraper();
    let server = ServerHandle::spawn(scraper).await;
    let client = reqwest::Client::new();

    let response = client
        .get(format!("{}/does-not-exist", server.base_url))
        .send()
        .await
        .expect("GET /does-not-exist");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_manager_skips_server() {
    let manager = MonitoringManager::new(PrometheusConfig::default()).expect("build disabled");
    assert!(!manager.is_enabled());
    let scraper = manager.into_scraper();
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("bind ephemeral port");
    let (_shutdown_tx, shutdown_rx) = watch::channel(false);
    let join = tokio::spawn(async move {
        air_elt_monitoring::server::serve_on_listener(scraper, listener, shutdown_rx)
            .await
            .expect("server task");
    });
    let outcome = tokio::time::timeout(Duration::from_millis(500), join)
        .await
        .expect("disabled server should return immediately");
    outcome.expect("server join");
}
