//! Test-only helper: provision a sandbox ClickHouse database for e2e
//! tests.
//!
//! Two modes:
//!
//! 1. If `AIR_ELT_TEST_CLICKHOUSE_URL` is set, connect there (the URL
//!    points at the HTTP endpoint, e.g. `http://localhost:8123`) and
//!    create a unique sandbox database per test. CI uses this mode.
//! 2. Otherwise launch a fresh ClickHouse container via testcontainers'
//!    `clickhouse` module.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::clickhouse::ClickHouse as ClickHouseImage;
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_CLICKHOUSE_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "clickhouse";

static CLICKHOUSE_BASE_URL: OnceCell<String> = OnceCell::const_new();

pub struct ClickHouseTestHandle {
    pub http: reqwest::Client,
    /// Base URL of the CH HTTP endpoint without database segment.
    /// Example: `http://127.0.0.1:8123`.
    pub url: String,
    /// Sandbox database name. Unique per handle.
    pub database: String,
}

impl ClickHouseTestHandle {
    /// Run an arbitrary SQL statement against the sandbox database.
    /// On error, the returned `String` is the CH error body (code + message).
    pub async fn exec(&self, sql: &str) -> Result<String, String> {
        exec_impl(&self.http, &self.url, Some(&self.database), sql).await
    }

    /// Run a SQL statement and return the response body as text.
    /// Bypasses the sandbox database (caller must qualify identifiers).
    pub async fn exec_root(&self, sql: &str) -> Result<String, String> {
        exec_impl(&self.http, &self.url, None, sql).await
    }
}

impl Drop for ClickHouseTestHandle {
    fn drop(&mut self) {
        let url = self.url.clone();
        let database = self.database.clone();
        // Best-effort sandbox cleanup. Blocking call inside Drop is OK
        // here — Drop runs on the test thread which already owns the
        // tokio runtime; building a separate blocking client avoids
        // borrowing the runtime mid-shutdown.
        let _ = std::thread::spawn(move || {
            if let Ok(c) = reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .build()
            {
                let _ = c
                    .post(&url)
                    .body(format!("DROP DATABASE IF EXISTS `{database}`"))
                    .send();
            }
        })
        .join();
    }
}

async fn exec_impl(
    http: &reqwest::Client,
    url: &str,
    database: Option<&str>,
    sql: &str,
) -> Result<String, String> {
    let mut req = http.post(url);
    if let Some(db) = database {
        req = req.query(&[("database", db)]);
    }
    let resp = req
        .body(sql.to_string())
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    if status.is_success() {
        Ok(body)
    } else {
        Err(body)
    }
}

async fn clickhouse_base_url() -> &'static String {
    CLICKHOUSE_BASE_URL
        .get_or_init(|| async move {
            if let Ok(external) = std::env::var(URL_VAR) {
                return trim_trailing_slash(&external);
            }
            let backend = detect_with_timeout(URL_VAR)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let socket = match backend {
                TestBackend::ExternalUrl => unreachable!("handled above"),
                TestBackend::Container { socket } => socket,
            };
            prepare_container_env(&socket);
            ryuk::ensure_session(&socket).await;
            let (sk, sv) = ryuk::session_label();
            info!("ensuring shared clickhouse container (reuse=Always, ryuk-managed)");
            let start_lock = crate::filelock::acquire_lock("clickhouse");
            let container = ClickHouseImage::default()
                .with_tag("24.8") // match CI — cf. ci.yml:210
                .with_env_var("CLICKHOUSE_SKIP_USER_SETUP", "1")
                .with_container_name(format!("air-elt-clickhouse-{sv}"))
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_label(sk, sv)
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .unwrap_or_else(|e| panic!("failed to start clickhouse container: {e}"));
            // Release the cross-process file lock before the host/port
            // probes so sibling test processes can proceed in parallel
            // (mirrors the pattern documented in pg.rs).
            drop(start_lock);
            let host = container
                .get_host()
                .await
                .unwrap_or_else(|e| panic!("clickhouse host: {e}"));
            let port = container
                .get_host_port_ipv4(8123)
                .await
                .unwrap_or_else(|e| panic!("clickhouse port: {e}"));
            format!("http://{host}:{port}")
        })
        .await
}

fn trim_trailing_slash(url: &str) -> String {
    url.trim_end_matches('/').to_string()
}

pub fn clickhouse_handle() -> Pin<Box<dyn Future<Output = ClickHouseTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let base = clickhouse_base_url().await;
        let database = format!(
            "air_elt_test_{}",
            Alphanumeric.sample_string(&mut rng(), 12).to_lowercase()
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest builder");
        let body = format!("CREATE DATABASE `{database}`");
        let resp = http
            .post(base)
            .body(body)
            .send()
            .await
            .unwrap_or_else(|e| panic!("CREATE DATABASE failed: {e}"));
        if !resp.status().is_success() {
            let txt = resp.text().await.unwrap_or_default();
            panic!("CREATE DATABASE returned non-2xx: {txt}");
        }
        ClickHouseTestHandle {
            http,
            url: base.clone(),
            database,
        }
    })
}
