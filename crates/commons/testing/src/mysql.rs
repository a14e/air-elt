//! Test-only helper: provision a MySQL `MySqlPool` for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_MYSQL_URL` is set, connect there and create a unique
//!    sandbox database per test. The database is dropped when the handle is
//!    dropped. CI uses this mode.
//! 2. Otherwise launch a fresh MySQL container via `testcontainers`.
//!
//! Note: in MySQL "schema" and "database" are synonyms — we expose the
//! sandbox name as `schema` to mirror the pg helper API.
//!
//! Across both modes the container (when used) and the resolved base URL
//! are cached in a process-wide `OnceCell`; subsequent tests reuse them
//! and only pay the cost of `CREATE DATABASE` + a fresh per-test pool.
//! Per-test pools are intentionally not cached — sqlx connection workers
//! are tied to the `#[tokio::test]` runtime that spawned them, and that
//! runtime is rebuilt for every test.

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use sqlx::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::mysql::Mysql as MySqlImage;
use tokio::sync::OnceCell;
use tracing::{info, warn};

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_MYSQL_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "mysql";

static MYSQL_BASE_URL: OnceCell<String> = OnceCell::const_new();
static SELF_HEAL_DONE: OnceCell<()> = OnceCell::const_new();

pub struct MySqlTestHandle {
    pub pool: MySqlPool,
    /// Base URL without database segment.
    pub url: String,
    /// The sandbox database name. Tests should fully-qualify table names
    /// (e.g. `format!("{}.users", handle.schema)`) when issuing DDL via the
    /// pool — the pool itself is already pinned to this database.
    pub schema: String,
    _cleanup: CleanupGuard,
}

impl MySqlTestHandle {
    /// URL pinned to the sandbox database — handy when tests need to spin
    /// up a *separate* pool against the same database.
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
                // Bound the cleanup so a hung server can't wedge the
                // test process forever. Stale dbs are reaped on the
                // next run via `drop_stale_test_databases`.
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

async fn mysql_base_url() -> &'static String {
    MYSQL_BASE_URL
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
            info!("ensuring shared mysql container (reuse=Always, ryuk-managed)");
            let container = MySqlImage::default()
                .with_tag("8.4")
                .with_container_name(format!("air-elt-mysql-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start mysql container failed");
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(3306)
                .await
                .expect("container port");
            drop(container);
            format!("mysql://root@{host}:{port}")
        })
        .await
}

pub async fn mysql_pool() -> MySqlTestHandle {
    let base_url = mysql_base_url().await;
    let db = random_db();
    info!(db = %db, "creating sandbox database");

    let bootstrap_pool = MySqlPoolOptions::new()
        .max_connections(2)
        .connect(base_url)
        .await
        .expect("connect to mysql failed");

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

    MySqlTestHandle {
        pool,
        url: base_url.clone(),
        schema: db,
        _cleanup: cleanup,
    }
}

/// Strip the `/<dbname>` path component from a mysql URL — we connect to the
/// server first to create the sandbox db, then re-open against that db.
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
    // mysql://user:pwd@host:port/dbname  →  ("mysql://user:pwd@host:port", Some("dbname"))
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

#[cfg(test)]
mod tests {
    use super::strip_db;

    #[test]
    fn strip_db_basic() {
        assert_eq!(
            strip_db("mysql://root@localhost:3306/myapp"),
            ("mysql://root@localhost:3306".into(), Some("myapp".into()))
        );
    }

    #[test]
    fn strip_db_no_db() {
        assert_eq!(
            strip_db("mysql://root@localhost:3306"),
            ("mysql://root@localhost:3306".into(), None)
        );
    }

    #[test]
    fn strip_db_with_query() {
        assert_eq!(
            strip_db("mysql://root@localhost:3306/myapp?ssl=true"),
            (
                "mysql://root@localhost:3306?ssl=true".into(),
                Some("myapp".into())
            )
        );
    }
}
