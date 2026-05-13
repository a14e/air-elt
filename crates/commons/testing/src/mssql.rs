//! Test-only helper: provision an MS SQL connection pool for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_MSSQL_URL` is set, connect there and create a unique
//!    sandbox database per test. The database is dropped when the handle is
//!    dropped. CI uses this mode.
//! 2. Otherwise launch a fresh MS SQL container via `testcontainers`
//!    using a generic image (no first-class `testcontainers-modules`
//!    integration exists for MS SQL beyond the example module).
//!
//! The container (when used) and resolved base URL are cached in a
//! process-wide `OnceCell`; subsequent tests reuse them and only pay for
//! `CREATE DATABASE` + a fresh per-test pool.
//!
//! Uses tiberius + bb8 for connections — sqlx 0.8 does not have an
//! MSSQL backend.

use std::future::Future;
use std::pin::Pin;

use air_elt_commons_mssql::pool::config_from_url;
use bb8::Pool;
use bb8_tiberius::ConnectionManager;
use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt, ReuseDirective};
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_MSSQL_URL";

const MSSQL_IMAGE: &str = "mcr.microsoft.com/mssql/server";
const MSSQL_TAG: &str = "2022-CU22-ubuntu-22.04";
const SA_PASSWORD: &str = "AirEltTest123!";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "mssql";

static MSSQL_BASE_URL: OnceCell<String> = OnceCell::const_new();

pub struct MssqlTestHandle {
    pub pool: Pool<ConnectionManager>,
    /// Base URL without the database segment.
    pub url: String,
    /// The sandbox database name.
    pub database: String,
}

impl MssqlTestHandle {
    pub fn url_with_database(&self) -> String {
        format!("{}/{}", self.url, self.database)
    }
}

async fn mssql_base_url() -> &'static String {
    MSSQL_BASE_URL
        .get_or_init(|| async {
            if let Ok(external) = std::env::var(URL_VAR) {
                let (base_url, _existing_db) = strip_db(&external);
                return base_url;
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
            info!("ensuring shared mssql container (reuse=Always, ryuk-managed)");
            let start_lock = crate::filelock::acquire_lock("mssql");
            let container = GenericImage::new(MSSQL_IMAGE, MSSQL_TAG)
                .with_env_var("ACCEPT_EULA", "Y")
                .with_env_var("MSSQL_SA_PASSWORD", SA_PASSWORD)
                .with_env_var("MSSQL_PID", "Developer")
                .with_container_name(format!("air-elt-mssql-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start mssql container failed");
            drop(start_lock);
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(1433)
                .await
                .expect("container port");
            drop(container);
            format!("mssql://sa:{SA_PASSWORD}@{host}:{port}")
        })
        .await
}

pub fn mssql_pool() -> Pin<Box<dyn Future<Output = MssqlTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let base_url = mssql_base_url().await;
        let db = random_db();
        info!(db = %db, "creating sandbox database");

        // SQL Server's amd64 image under Rosetta on Apple Silicon needs
        // 60-180s to surface a TCP listener even after the container is
        // "up". 240s budget covers the cold-start path with margin; native
        // x86 CI finishes in ~30s.
        let bootstrap_pool = retry_connect(base_url, 240).await;

        let mut conn = bootstrap_pool.get().await.expect("bootstrap conn");
        let create = format!("CREATE DATABASE [{db}]");
        conn.execute(&create, &[])
            .await
            .expect("create sandbox database failed");
        drop(conn);

        let scoped_url = format!("{}/{}", base_url, db);
        let pool = retry_connect(&scoped_url, 30).await;

        MssqlTestHandle {
            pool,
            url: base_url.clone(),
            database: db,
        }
    })
}

fn strip_db(url: &str) -> (String, Option<String>) {
    // mssql://user:pass@host:port/dbname  →  ("mssql://user:pass@host:port", Some("dbname"))
    if let Some(qmark) = url.find('?') {
        let (head, tail) = url.split_at(qmark);
        let (base, db) = strip_db_from_path(head);
        (format!("{base}{tail}"), db)
    } else {
        strip_db_from_path(url)
    }
}

fn strip_db_from_path(head: &str) -> (String, Option<String>) {
    let scheme_split = head.find("://").map(|i| i + 3).unwrap_or(0);
    let (scheme, rest) = head.split_at(scheme_split);
    if let Some(slash) = rest.find('/') {
        let (auth_host, slash_db) = rest.split_at(slash);
        let db = slash_db.trim_start_matches('/');
        let db = if db.is_empty() {
            None
        } else {
            Some(db.to_string())
        };
        (format!("{scheme}{auth_host}"), db)
    } else {
        (head.to_string(), None)
    }
}

fn random_db() -> String {
    let suffix = Alphanumeric
        .sample_string(&mut rng(), 8)
        .to_ascii_lowercase();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("test_{now}_{suffix}")
}

async fn retry_connect(url: &str, deadline_secs: u64) -> Pool<ConnectionManager> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
    let mut last_err: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let config = match config_from_url(url) {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(e.to_string());
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                continue;
            }
        };
        let manager = ConnectionManager::new(config);
        let pool = match bb8::Pool::builder()
            .max_size(5)
            .connection_timeout(std::time::Duration::from_secs(5))
            .build(manager)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                last_err = Some(e.to_string());
                tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
                continue;
            }
        };
        // `Pool::builder().build()` does NOT actually open a TCP socket — it
        // just stores the manager. Probe a real connection here so we
        // genuinely wait for SQL Server to be ready, not just for tiberius
        // to parse the URL.
        let probe_ok = {
            match pool.get().await {
                Ok(mut conn) => {
                    let ok = conn.simple_query("SELECT 1").await.is_ok();
                    drop(conn);
                    ok
                }
                Err(e) => {
                    last_err = Some(format!("pool.get: {e}"));
                    false
                }
            }
        };
        if probe_ok {
            return pool;
        }
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
    panic!("connect to mssql failed within {deadline_secs}s: {last_err:?}");
}

#[cfg(test)]
mod tests {
    use super::strip_db;

    #[test]
    fn strip_db_basic() {
        assert_eq!(
            strip_db("mssql://sa:pass@localhost:1433/myapp"),
            (
                "mssql://sa:pass@localhost:1433".into(),
                Some("myapp".into())
            )
        );
    }

    #[test]
    fn strip_db_no_db() {
        assert_eq!(
            strip_db("mssql://sa:pass@localhost:1433"),
            ("mssql://sa:pass@localhost:1433".into(), None)
        );
    }
}
