//! Test-only helper: provision a postgres `PgPool` for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_PG_URL` is set, connect there and create a unique
//!    sandbox schema per test. CI uses this mode (GHA `services.postgres`).
//! 2. Otherwise launch a postgres container via `testcontainers` in
//!    `ReuseDirective::Always` mode — labelled with the current ryuk
//!    session so it's shared across every test process of one cargo
//!    invocation and reaped automatically when the last process exits.
//!
//! Per-test we create a fresh sandbox schema and a fresh `PgPool` against
//! it. Pools are intentionally NOT cached across tests — sqlx connection
//! workers are tied to the tokio runtime that spawned them, and
//! `#[tokio::test]` builds a fresh runtime for each test.
//!
//! Locally ryuk reaps the entire container when the cargo session
//! ends. There is no per-test cleanup hook — schemas have unique
//! random names and die with the container.

use std::future::Future;
use std::pin::Pin;

use rand::distr::{Alphanumeric, SampleString};
use rand::rng;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::Mount;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ImageExt, ReuseDirective};
use testcontainers_modules::postgres::Postgres as PgImage;
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_PG_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "pg";

static PG_BASE_URL: OnceCell<String> = OnceCell::const_new();

pub struct PgTestHandle {
    pub pool: PgPool,
    pub url: String,
    pub schema: String,
}

impl PgTestHandle {
    pub fn url_with_search_path(&self) -> String {
        format!("{}?options=-c%20search_path%3D{}", self.url, self.schema)
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
            // tmpfs on the data dir + relaxed durability flags trade
            // crash-safety for raw throughput. Acceptable in tests: the
            // container is reaped at session end, so on-disk state has no
            // value.
            // Lock held only across the create-or-reuse race window
            // (`start().await`). Once the daemon has the container,
            // other processes resolve via `reuse=Always` instantly.
            let start_lock = crate::filelock::acquire_lock("pg");
            let container = PgImage::default()
                .with_tag("16-alpine")
                .with_container_name(format!("air-elt-pg-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
                .with_cmd([
                    "postgres",
                    "-c",
                    "fsync=off",
                    "-c",
                    "synchronous_commit=off",
                    "-c",
                    "full_page_writes=off",
                ])
                .with_reuse(ReuseDirective::Always)
                .start()
                .await
                .expect("start postgres container failed");
            drop(start_lock);
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

pub fn pg_pool() -> Pin<Box<dyn Future<Output = PgTestHandle> + Send + 'static>> {
    Box::pin(async move {
        let base_url = pg_base_url().await;
        let schema = random_schema();
        info!(schema = %schema, "creating sandbox schema");

        let bootstrap_pool = PgPoolOptions::new()
            .max_connections(2)
            .connect(base_url)
            .await
            .expect("connect to postgres failed");

        let create = format!("CREATE SCHEMA \"{schema}\"");
        sqlx::query(&create)
            .execute(&bootstrap_pool)
            .await
            .expect("create sandbox schema failed");

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
        }
    })
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
