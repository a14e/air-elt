//! Test-only helper: provision a postgres `PgPool` for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_PG_URL` is set, connect there and create a unique
//!    sandbox schema per test. The schema is dropped when the handle is
//!    dropped. CI uses this mode (GHA `services.postgres`).
//! 2. Otherwise launch a fresh postgres container via `testcontainers`.

use rand::distr::Alphanumeric;
use rand::{Rng, rng};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres as PgImage;
use tracing::{info, warn};

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};

const URL_VAR: &str = "AIR_ELT_TEST_PG_URL";

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

enum CleanupGuard {
    ExternalSchema {
        pool: PgPool,
        schema: String,
    },
    Container {
        _container: Box<ContainerAsync<PgImage>>,
    },
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        match self {
            CleanupGuard::ExternalSchema { pool, schema } => {
                let pool = pool.clone();
                let schema = schema.clone();
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
                        match tokio::time::timeout(std::time::Duration::from_secs(5), drop_fut)
                            .await
                        {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                warn!(error = %e, schema, "failed to drop test schema");
                            }
                            Err(_) => {
                                warn!(schema, "drop schema timed out — relying on self-heal");
                            }
                        }
                        let _ =
                            tokio::time::timeout(std::time::Duration::from_secs(2), pool.close())
                                .await;
                    });
                });
                if let Err(e) = join.join() {
                    warn!(?e, "cleanup thread panicked");
                }
            }
            CleanupGuard::Container { .. } => {}
        }
    }
}

pub async fn pg_pool() -> PgTestHandle {
    if let Ok(external) = std::env::var(URL_VAR) {
        return external_with_sandbox(&external).await;
    }
    let backend = detect_with_timeout(URL_VAR)
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    match backend {
        TestBackend::ExternalUrl => unreachable!("handled above"),
        TestBackend::Container { socket } => {
            prepare_container_env(&socket);
            spawn_container().await
        }
    }
}

async fn external_with_sandbox(url: &str) -> PgTestHandle {
    let schema = random_schema();
    info!(schema = %schema, "creating sandbox schema on external postgres");

    let bootstrap_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(url)
        .await
        .expect("connect to AIR_ELT_TEST_PG_URL failed");

    drop_stale_test_schemas(&bootstrap_pool, 24 * 3600).await;

    let create = format!("CREATE SCHEMA \"{schema}\"");
    sqlx::query(&create)
        .execute(&bootstrap_pool)
        .await
        .expect("create sandbox schema failed");

    let cleanup = CleanupGuard::ExternalSchema {
        pool: bootstrap_pool.clone(),
        schema: schema.clone(),
    };

    let scoped_url = format!("{url}?options=-c%20search_path%3D{schema}");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&scoped_url)
        .await
        .expect("connect to sandbox schema failed");

    PgTestHandle {
        pool,
        url: url.to_string(),
        schema,
        _cleanup: cleanup,
    }
}

async fn spawn_container() -> PgTestHandle {
    info!("starting ephemeral postgres container (AIR_ELT_TEST_PG_URL not set)");
    let container = PgImage::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres container failed");

    let host = container.get_host().await.expect("container host");
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .expect("container port");

    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .expect("connect to containerised postgres failed");

    PgTestHandle {
        pool,
        url,
        schema: "public".to_string(),
        _cleanup: CleanupGuard::Container {
            _container: Box::new(container),
        },
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
    let suffix: String = rng()
        .sample_iter(&Alphanumeric)
        .take(8)
        .map(|c| (c as char).to_ascii_lowercase())
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("test_{now}_{suffix}")
}
