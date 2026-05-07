//! Test-only helper: provision a CockroachDB `PgPool` for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_COCKROACHDB_URL` is set, connect there and create a
//!    unique sandbox database per test. The database is dropped when the
//!    handle is dropped. CI uses this mode (a `cockroachdb` container is
//!    spun up by the workflow).
//! 2. Otherwise launch a fresh CockroachDB container via `testcontainers`
//!    using the generic image (no first-class `testcontainers-modules`
//!    integration exists for cockroach yet).
//!
//! The container (when used) and resolved base URL are cached in a
//! process-wide `OnceCell`; subsequent tests reuse them and only pay for
//! `CREATE DATABASE` + a fresh per-test pool.
//!
//! CockroachDB speaks the Postgres wire protocol; the returned pool is a
//! `sqlx::PgPool`. URLs follow the `postgres://root@host:26257/<db>?sslmode=disable`
//! form, matching what `air-elt-commons-pg::pool::connect` understands.

use std::future::Future;
use std::pin::Pin;

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::ContainerPort;
use testcontainers::runners::AsyncRunner;
use testcontainers::{GenericImage, ImageExt, ReuseDirective};
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_COCKROACHDB_URL";

/// CockroachDB image tag we test against. Pinned for reproducibility.
const COCKROACH_IMAGE: &str = "cockroachdb/cockroach";
const COCKROACH_TAG: &str = "v25.1.0";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "cockroach";

static COCKROACH_BASE_URL: OnceCell<String> = OnceCell::const_new();

pub struct CockroachTestHandle {
    pub pool: PgPool,
    /// Base URL without the `/<db>` segment.
    pub url: String,
    /// Sandbox database name. Tests can fully-qualify table names as
    /// `format!("{}.public.users", handle.database)` if they need cross-db
    /// statements; the pool itself is already pinned to this database.
    pub database: String,
}

impl CockroachTestHandle {
    /// URL pinned to the sandbox database — handy when tests need to spin
    /// up a *separate* pool against the same database.
    pub fn url_with_database(&self) -> String {
        url_with_db(&self.url, &self.database)
    }
}

async fn cockroach_base_url() -> &'static String {
    COCKROACH_BASE_URL
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
            info!("ensuring shared cockroach container (reuse=Always, ryuk-managed)");
            let start_lock = crate::filelock::acquire_lock("cockroach");
            // No `WaitFor` log marker: CockroachDB's startup banner lands
            // on stderr in some image builds and stdout in others, and a
            // mismatched marker leaves testcontainers blocked on the
            // global startup timeout. Instead we let the container start
            // immediately and probe the SQL listener on the host side
            // until it accepts `SELECT 1`. The retry-loop cost is
            // amortised across every test that follows.
            let image = GenericImage::new(COCKROACH_IMAGE, COCKROACH_TAG)
                .with_exposed_port(ContainerPort::Tcp(26257))
                // In-memory store: cockroach's native equivalent of tmpfs.
                // Container is reaped at session end so durability has no
                // value here.
                .with_cmd([
                    "start-single-node",
                    "--insecure",
                    "--store=type=mem,size=1GiB",
                ])
                .with_container_name(format!("air-elt-cockroach-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_reuse(ReuseDirective::Always);
            let container = image
                .start()
                .await
                .expect("start cockroach container failed");
            drop(start_lock);
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(26257)
                .await
                .expect("container port");
            let base_url = format!("postgres://root@{host}:{port}?sslmode=disable");
            wait_for_ready(&base_url).await;
            drop(container);
            base_url
        })
        .await
}

pub fn cockroach_pool() -> Pin<Box<dyn Future<Output = CockroachTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let base_url = cockroach_base_url().await;
        let db = random_db();
        info!(db = %db, "creating sandbox database");

        // Bootstrap connection goes against `defaultdb` so we can issue
        // `CREATE DATABASE`. The bootstrap pool is per-test (cheap: the
        // container is already up). Reuse query string from the user's URL
        // if present.
        let bootstrap_url = url_with_db(base_url, "defaultdb");
        let bootstrap_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(&bootstrap_url)
            .await
            .expect("connect to cockroach failed");

        let create = format!("CREATE DATABASE \"{db}\"");
        sqlx::query(&create)
            .execute(&bootstrap_pool)
            .await
            .expect("create sandbox database failed");

        let scoped_url = url_with_db(base_url, &db);
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .connect(&scoped_url)
            .await
            .expect("connect to sandbox database failed");

        CockroachTestHandle {
            pool,
            url: base_url.clone(),
            database: db,
        }
    })
}

/// Strip the `/<dbname>` path component from a Postgres-style URL — mirroring
/// the helper in `mysql.rs`. Cockroach connection strings use the same shape
/// as Postgres.
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

fn url_with_db(base_url: &str, db: &str) -> String {
    if let Some((head, tail)) = base_url.split_once('?') {
        format!("{head}/{db}?{tail}")
    } else {
        format!("{base_url}/{db}")
    }
}

/// Probe the SQL listener until it accepts a `SELECT 1`. Runs once per
/// process during container infra init. The deadline is generous because
/// `cargo test` runs every test binary in parallel — when several
/// binaries each kick off a Cockroach container at the same time the
/// container daemon gets backed up and individual starts take longer
/// than they would in isolation.
async fn wait_for_ready(base_url: &str) {
    let probe_url = url_with_db(base_url, "defaultdb");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    let mut last_err: Option<sqlx::Error> = None;
    while std::time::Instant::now() < deadline {
        match PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(1))
            .connect(&probe_url)
            .await
        {
            Ok(pool) => {
                let ok = sqlx::query("SELECT 1").execute(&pool).await.is_ok();
                let _ = pool.close().await;
                if ok {
                    return;
                }
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    }
    panic!("cockroach did not become ready within 180s: {:?}", last_err);
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
    use super::*;

    #[test]
    fn strip_db_basic() {
        assert_eq!(
            strip_db("postgres://root@localhost:26257/foo"),
            ("postgres://root@localhost:26257".into(), Some("foo".into()))
        );
    }

    #[test]
    fn strip_db_with_query() {
        assert_eq!(
            strip_db("postgres://root@localhost:26257/foo?sslmode=disable"),
            (
                "postgres://root@localhost:26257?sslmode=disable".into(),
                Some("foo".into())
            )
        );
    }

    #[test]
    fn strip_db_no_db() {
        assert_eq!(
            strip_db("postgres://root@localhost:26257?sslmode=disable"),
            (
                "postgres://root@localhost:26257?sslmode=disable".into(),
                None
            )
        );
    }

    #[test]
    fn url_with_db_appends_path() {
        assert_eq!(
            url_with_db("postgres://root@h:1?sslmode=disable", "x"),
            "postgres://root@h:1/x?sslmode=disable"
        );
        assert_eq!(
            url_with_db("postgres://root@h:1", "x"),
            "postgres://root@h:1/x"
        );
    }
}
