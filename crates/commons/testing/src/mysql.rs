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

use std::future::Future;
use std::pin::Pin;

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use sqlx::MySqlPool;
use sqlx::mysql::MySqlPoolOptions;
use testcontainers::core::Mount;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::mysql::Mysql as MySqlImage;
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_MYSQL_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "mysql";

static MYSQL_BASE_URL: OnceCell<String> = OnceCell::const_new();

pub struct MySqlTestHandle {
    pub pool: MySqlPool,
    /// Base URL without database segment.
    pub url: String,
    /// The sandbox database name. Tests should fully-qualify table names
    /// (e.g. `format!("{}.users", handle.schema)`) when issuing DDL via the
    /// pool — the pool itself is already pinned to this database.
    pub schema: String,
}

impl MySqlTestHandle {
    /// URL pinned to the sandbox database — handy when tests need to spin
    /// up a *separate* pool against the same database.
    pub fn url_with_database(&self) -> String {
        format!("{}/{}", self.url, self.schema)
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
            let start_lock = crate::filelock::acquire_lock("mysql");
            // tmpfs on the data dir + relaxed durability flags trade
            // crash-safety for raw throughput. Acceptable in tests: the
            // container is reaped at session end, so on-disk state has no
            // value.
            let container = MySqlImage::default()
                .with_tag("8.4")
                .with_container_name(format!("air-elt-mysql-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_mount(Mount::tmpfs_mount("/var/lib/mysql"))
                .with_cmd([
                    "mysqld",
                    "--innodb-flush-log-at-trx-commit=0",
                    "--innodb-doublewrite=0",
                    "--sync-binlog=0",
                    "--skip-log-bin",
                    // nextest spawns ~one process per test (≈800 here),
                    // each opening its own pool. Default `max_connections=151`
                    // and `max_connect_errors=100` are blown through and
                    // mysqld starts dropping/blocking connections.
                    "--max-connections=500",
                    "--max-connect-errors=100000",
                ])
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start mysql container failed");
            drop(start_lock);
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

pub fn mysql_pool() -> Pin<Box<dyn Future<Output = MySqlTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let base_url = mysql_base_url().await;
        let db = random_db();
        info!(db = %db, "creating sandbox database");

        // mysqld drops new TCP connections (UnexpectedEof) when many
        // test processes hit it cold simultaneously (nextest runs one
        // process per test). Retry the bootstrap connect for up to
        // 30s before giving up. Each iteration sleeps briefly to let
        // the server work through the connect-error backoff window.
        let bootstrap_pool = retry_connect(base_url, 30).await;

        let create = format!("CREATE DATABASE `{db}`");
        sqlx::query(&create)
            .execute(&bootstrap_pool)
            .await
            .expect("create sandbox database failed");

        let scoped_url = format!("{}/{}", base_url, db);
        let pool = retry_connect(&scoped_url, 30).await;

        MySqlTestHandle {
            pool,
            url: base_url.clone(),
            schema: db,
        }
    })
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

async fn retry_connect(url: &str, deadline_secs: u64) -> sqlx::MySqlPool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(deadline_secs);
    let mut last_err: Option<sqlx::Error> = None;
    while std::time::Instant::now() < deadline {
        match MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(2))
            .connect(url)
            .await
        {
            Ok(pool) => return pool,
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
    panic!("connect to mysql failed within {deadline_secs}s: {last_err:?}");
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
