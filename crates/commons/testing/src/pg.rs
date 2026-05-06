//! Test-only helper: provision a postgres `PgPool` for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_PG_URL` is set, connect there and create a unique
//!    sandbox schema per test. The schema is dropped when the handle is
//!    dropped. CI uses this mode (GHA `services.postgres`).
//! 2. Otherwise launch a postgres container via `testcontainers` in
//!    `ReuseDirective::Always` mode — labelled with the current ryuk
//!    session so it's shared across every test process of one cargo
//!    invocation and reaped automatically when the last process exits.
//!
//! Per-test we still create a fresh sandbox schema and a fresh `PgPool`
//! against it. Pools are intentionally NOT cached across tests — sqlx
//! connection workers are tied to the tokio runtime that spawned them, and
//! `#[tokio::test]` builds a fresh runtime for each test.

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::postgres::Postgres as PgImage;
use tokio::sync::OnceCell;
use tracing::{info, warn};

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_PG_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "pg";

static PG_BASE_URL: OnceCell<String> = OnceCell::const_new();
static SELF_HEAL_DONE: OnceCell<()> = OnceCell::const_new();

pub struct PgTestHandle {
    pub pool: PgPool,
    pub url: String,
    pub schema: String,
    _cleanup: CleanupGuard,
}

impl PgTestHandle {
    pub fn url_with_search_path(&self) -> String {
        format!("{}?options=-c%20search_path%3D{}", self.url, self.schema)
    }
}

/// Drops the per-test sandbox schema. Owns the bootstrap pool that issued
/// the `CREATE SCHEMA` so cleanup goes through the same connection pool.
struct CleanupGuard {
    bootstrap: PgPool,
    schema: String,
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        let pool = self.bootstrap.clone();
        let schema = self.schema.clone();
        let join = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build cleanup runtime");
            rt.block_on(async move {
                // Bound the cleanup so a hung server can't wedge the
                // test process forever. Stale schemas are reaped on
                // the next run via `drop_stale_test_schemas`.
                let stmt = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
                let drop_fut = sqlx::query(&stmt).execute(&pool);
                match tokio::time::timeout(std::time::Duration::from_secs(5), drop_fut).await {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        warn!(error = %e, schema, "failed to drop test schema");
                    }
                    Err(_) => {
                        warn!(schema, "drop schema timed out — relying on self-heal");
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

async fn pg_base_url() -> &'static String {
    PG_BASE_URL
        .get_or_init(|| async {
            if let Ok(external) = std::env::var(URL_VAR) {
                return external;
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
            info!("ensuring shared postgres container (reuse=Always, ryuk-managed)");
            let container = PgImage::default()
                .with_tag("16-alpine")
                .with_container_name(format!("air-elt-pg-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start postgres container failed");
            let host = container.get_host().await.expect("container host");
            let port = container
                .get_host_port_ipv4(5432)
                .await
                .expect("container port");
            // Container handle is intentionally dropped — ryuk holds its
            // lifetime via the session label.
            drop(container);
            format!("postgres://postgres:postgres@{host}:{port}/postgres")
        })
        .await
}

pub async fn pg_pool() -> PgTestHandle {
    let base_url = pg_base_url().await;
    let schema = random_schema();
    info!(schema = %schema, "creating sandbox schema");

    let bootstrap_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(base_url)
        .await
        .expect("connect to postgres failed");

    SELF_HEAL_DONE
        .get_or_init(|| async {
            drop_stale_test_schemas(&bootstrap_pool, 24 * 3600).await;
        })
        .await;

    let create = format!("CREATE SCHEMA \"{schema}\"");
    sqlx::query(&create)
        .execute(&bootstrap_pool)
        .await
        .expect("create sandbox schema failed");

    let cleanup = CleanupGuard {
        bootstrap: bootstrap_pool.clone(),
        schema: schema.clone(),
    };

    let scoped_url = format!("{}?options=-c%20search_path%3D{}", base_url, schema);
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&scoped_url)
        .await
        .expect("connect to sandbox schema failed");

    PgTestHandle {
        pool,
        url: base_url.clone(),
        schema,
        _cleanup: cleanup,
    }
}

async fn drop_stale_test_schemas(pool: &PgPool, max_age_secs: u64) {
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
        .saturating_sub(max_age_secs);

    let rows: Vec<(String,)> = match sqlx::query_as(
        "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE 'test\\_%' ESCAPE '\\'",
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "could not enumerate test_* schemas for self-heal");
            return;
        }
    };

    for (schema_name,) in rows {
        let ts = schema_name
            .strip_prefix("test_")
            .and_then(|s| s.split_once('_'))
            .and_then(|(ts_str, _)| ts_str.parse::<u64>().ok());
        let Some(ts) = ts else {
            continue;
        };
        if ts >= cutoff {
            continue;
        }
        let stmt = format!("DROP SCHEMA IF EXISTS \"{schema_name}\" CASCADE");
        if let Err(e) = sqlx::query(&stmt).execute(pool).await {
            tracing::warn!(error = %e, schema = %schema_name, "failed to drop stale schema");
        } else {
            tracing::debug!(schema = %schema_name, "self-healed stale test schema");
        }
    }
}

fn random_schema() -> String {
    let suffix = Alphanumeric
        .sample_string(&mut rng(), 8)
        .to_ascii_lowercase();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("test_{now}_{suffix}")
}
