//! Test-only helper: provision a MariaDB pool for e2e tests.
//!
//! MariaDB speaks the MySQL wire protocol, so we reuse `sqlx::MySqlPool`.
//! The helper is a thin parallel of [`crate::mysql`]; it exists separately
//! because (a) the testcontainers image differs and (b) MariaDB has small
//! divergences (legacy `VALUES()` UPSERT, native `UUID` type from 10.7+)
//! that we want to exercise without conflating them with stock MySQL.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_MARIADB_URL` is set, connect there and create a
//!    unique sandbox database per test.
//! 2. Otherwise launch a fresh MariaDB container via `testcontainers`.
//!
//! The container (when used) and the resolved base URL are cached in a
//! process-wide `OnceCell`; subsequent tests reuse them. Per-test pools
//! stay per-test since sqlx connection workers are tied to the spawning
//! `#[tokio::test]` runtime.

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use sqlx::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::mariadb::Mariadb as MariaDbImage;
use tokio::sync::OnceCell;
use tracing::{info, warn};

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_MARIADB_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "mariadb";

static MARIADB_BASE_URL: OnceCell<String> = OnceCell::const_new();
static SELF_HEAL_DONE: OnceCell<()> = OnceCell::const_new();

pub struct MariaDbTestHandle {
    pub pool: MySqlPool,
    /// Base URL without database segment.
    pub url: String,
    /// Sandbox database name (called "schema" to mirror the pg helper).
    pub schema: String,
    _cleanup: CleanupGuard,
}

impl MariaDbTestHandle {
    pub fn url_with_database(&self) -> String {
        format!("{}/{}", self.url, self.schema)
    }
}

struct CleanupGuard {
    bootstrap: MySqlPool,
    db: String,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let pool = self.bootstrap.clone();
        let db = std::mem::take(&mut self.db);
        let join = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build cleanup runtime");
            rt.block_on(async move {
                let stmt = format!("DROP DATABASE IF EXISTS `{db}`");
                let drop_fut = sqlx::query(&stmt).execute(&pool);
                match tokio::time::timeout(std::time::Duration::from_secs(5), drop_fut).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        warn!(error = %e, db, "failed to drop test database");
                    }
                    Err(_) => {
                        warn!(db, "drop database timed out — relying on self-heal");
                    }
                }
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), pool.close()).await;
            });
        });
        if let Err(e) = join.join() {
            warn!(?e, "cleanup thread panicked");
        }
    }
}

async fn mariadb_base_url() -> &'static String {
    MARIADB_BASE_URL
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
            info!("ensuring shared mariadb container (reuse=Always, ryuk-managed)");
            // The module's default tag already targets a recent MariaDB
            // (≥ 11.x) which ships native UUID (added in 10.7). Overriding
            // with `.with_tag()` can race the readiness probe — leave it.
            let container = MariaDbImage::default()
                .with_container_name(format!("air-elt-mariadb-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start mariadb container failed");
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(3306)
                .await
                .expect("container port");
            let base_url = format!("mysql://root@{host}:{port}");
            // The official mariadb image starts mariadbd briefly *during*
            // init to run bootstrap SQL, then restarts it in the foreground.
            // testcontainers matches the first "ready for connections"
            // message and unblocks before that restart, so a short connect
            // window can race the server going down. Probe steady state
            // once during infra init; subsequent tests connect against the
            // settled server.
            wait_for_steady_state(&base_url).await;
            drop(container);
            base_url
        })
        .await
}

pub async fn mariadb_pool() -> MariaDbTestHandle {
    let base_url = mariadb_base_url().await;
    let db = random_db();
    info!(db = %db, "creating sandbox database");

    let bootstrap_pool = MySqlPoolOptions::new()
        .max_connections(2)
        .connect(base_url)
        .await
        .expect("connect to mariadb failed");

    SELF_HEAL_DONE
        .get_or_init(|| async {
            drop_stale_test_databases(&bootstrap_pool, 24 * 3600).await;
        })
        .await;

    let create = format!("CREATE DATABASE `{db}`");
    sqlx::query(&create)
        .execute(&bootstrap_pool)
        .await
        .expect("create sandbox database failed");

    let cleanup = CleanupGuard {
        bootstrap: bootstrap_pool.clone(),
        db: db.clone(),
    };

    let scoped_url = format!("{}/{}", base_url, db);
    let pool = MySqlPoolOptions::new()
        .max_connections(5)
        .connect(&scoped_url)
        .await
        .expect("connect to sandbox database failed");

    MariaDbTestHandle {
        pool,
        url: base_url.clone(),
        schema: db,
        _cleanup: cleanup,
    }
}

fn strip_db(url: &str) -> (String, Option<String>) {
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

/// Probe the URL until two consecutive fresh pools can both `SELECT 1`.
/// The first success may catch the temp init mariadbd; the second confirms
/// we're talking to the foreground server. Runs once per process during
/// container infra init.
async fn wait_for_steady_state(base_url: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let mut last_err: Option<sqlx::Error> = None;
    while std::time::Instant::now() < deadline {
        match MySqlPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(3))
            .connect(base_url)
            .await
        {
            Ok(pool) => {
                let ok = sqlx::query("SELECT 1").execute(&pool).await.is_ok();
                let _ = pool.close().await;
                if ok {
                    // Second probe confirms steady state across the
                    // init-restart boundary.
                    if let Ok(probe) = MySqlPoolOptions::new()
                        .max_connections(1)
                        .acquire_timeout(std::time::Duration::from_secs(3))
                        .connect(base_url)
                        .await
                    {
                        let _ = probe.close().await;
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    panic!(
        "mariadb did not reach steady state within 60s: {:?}",
        last_err
    );
}

async fn drop_stale_test_databases(pool: &MySqlPool, max_age_secs: u64) {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(max_age_secs);

    let rows: Vec<(String,)> = match sqlx::query_as(
        "SELECT schema_name FROM information_schema.schemata \
         WHERE schema_name LIKE 'test\\_%' ESCAPE '\\\\'",
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate test_* databases for self-heal");
            return;
        }
    };

    for (db,) in rows {
        let ts = db
            .strip_prefix("test_")
            .and_then(|s| s.split_once('_'))
            .and_then(|(ts_str, _)| ts_str.parse::<u64>().ok());
        let Some(ts) = ts else {
            continue;
        };
        if ts >= cutoff {
            continue;
        }
        let stmt = format!("DROP DATABASE IF EXISTS `{db}`");
        if let Err(e) = sqlx::query(&stmt).execute(pool).await {
            tracing::warn!(error = %e, db = %db, "failed to drop stale database");
        } else {
            tracing::debug!(db = %db, "self-healed stale test database");
        }
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
