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
use testcontainers::core::build::build_options::BuildImageOptions;
use testcontainers::core::{ContainerPort, Mount, WaitFor};
use testcontainers::runners::{AsyncBuilder, AsyncRunner};
use testcontainers::{GenericBuildableImage, ImageExt, ReuseDirective};
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

const URL_VAR: &str = "AIR_ELT_TEST_PG_URL";
const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "pg";

/// Image we run for the postgres test handle.
///
/// Custom image built locally from `docker/pg-hll/Dockerfile` — stock
/// `postgres:16` plus the `postgresql-hll` extension. Lighter than
/// `citusdata/citus` (which has a per-connection routing layer that
/// dominates nextest runtime where each test is its own process) and
/// drops the Citus-specific URL handling (`?sslmode=disable`,
/// `search_path=…,public`).
const PG_IMAGE: &str = "air-elt-pg-hll";
const PG_TAG: &str = "16";

/// Path to the Dockerfile directory relative to the workspace root.
/// Used by `ensure_image_built()` to build on first use.
const DOCKERFILE_DIR: &str = "crates/commons/testing/docker/pg-hll";

static PG_BASE_URL: OnceCell<String> = OnceCell::const_new();

pub struct PgTestHandle {
    pub pool: PgPool,
    pub url: String,
    pub schema: String,
}

impl PgTestHandle {
    pub fn url_with_search_path(&self) -> String {
        // Include `public` so extension types (e.g. `hll`, installed
        // at container init in the public schema) resolve from
        // sandbox schemas without explicit qualification.
        let separator = if self.url.contains('?') { '&' } else { '?' };
        format!(
            "{}{separator}options=-c%20search_path%3D{}%2Cpublic",
            self.url, self.schema
        )
    }
}

async fn pg_base_url() -> &'static String {
    PG_BASE_URL
        .get_or_init(|| async {
            if let Ok(external) = std::env::var(URL_VAR) {
                // CI / external-PG mode still needs the HLL extension
                // installed (the e2e suite assumes it). Install is
                // idempotent (`IF NOT EXISTS`); if the server doesn't
                // ship the extension shared object the call fails
                // loud, which is the right CI signal.
                install_hll_extension(&external).await;
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
            // Build the custom pg+hll image once per session.
            // `BuildImageOptions::with_skip_if_exists(true)` makes
            // the build a no-op when the tag is already present, so
            // every nextest process passes through cheaply after the
            // first one does the actual work. The build itself is
            // serialised across processes by the start_lock above.
            let dockerfile = workspace_root().join(DOCKERFILE_DIR).join("Dockerfile");
            let _built = GenericBuildableImage::new(PG_IMAGE, PG_TAG)
                .with_dockerfile(dockerfile)
                .build_image_with(BuildImageOptions::new().with_skip_if_exists(true))
                .await
                .expect("build air-elt-pg-hll image");
            let container = _built
                .with_exposed_port(ContainerPort::Tcp(5432))
                .with_wait_for(WaitFor::message_on_stderr(
                    "database system is ready to accept connections",
                ))
                .with_container_name(format!("air-elt-pg-{sv}"))
                .with_label(sk, sv)
                .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
                .with_env_var("POSTGRES_USER", "postgres")
                .with_env_var("POSTGRES_PASSWORD", "postgres")
                .with_env_var("POSTGRES_DB", "postgres")
                .with_env_var("POSTGRES_HOST_AUTH_METHOD", "trust")
                .with_mount(Mount::tmpfs_mount("/var/lib/postgresql/data"))
                .with_cmd([
                    "postgres",
                    "-c",
                    "fsync=off",
                    "-c",
                    "synchronous_commit=off",
                    "-c",
                    "full_page_writes=off",
                    // Bump the backend cap so workspace runs (esp.
                    // nextest, one process per test) don't hit the
                    // PG default `max_connections=100`.
                    "-c",
                    "max_connections=500",
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
            // Stock postgres:16 doesn't ship SSL; plain URL is fine.
            let base_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
            install_hll_extension(&base_url).await;
            base_url
        })
        .await
}

/// Walk up from `CARGO_MANIFEST_DIR` (the commons-testing crate dir)
/// to the workspace root by looking for the workspace `Cargo.toml`.
/// We need an absolute path because the build runs in whatever cwd
/// the test binary inherited.
fn workspace_root() -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    loop {
        let candidate = p.join("Cargo.toml");
        if candidate.exists() {
            let txt = std::fs::read_to_string(&candidate).unwrap_or_default();
            if txt.contains("[workspace]") {
                return p;
            }
        }
        if !p.pop() {
            panic!("workspace root not found above {DOCKERFILE_DIR}");
        }
    }
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

        // Add `public` to the search path so extension types (e.g.
        // `hll`) installed at container init resolve from sandbox
        // schemas without explicit qualification.
        let separator = if base_url.contains('?') { '&' } else { '?' };
        let scoped_url =
            format!("{base_url}{separator}options=-c%20search_path%3D{schema}%2Cpublic");
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

/// Install `hll` once per container lifetime. Citus ships the extension
/// as a preinstalled shared object but does NOT run `CREATE EXTENSION`
/// automatically — that has to happen per-database. We do it on
/// `postgres` (the bootstrap database) so any sandbox schema in that
/// database inherits the type. `IF NOT EXISTS` makes this idempotent
/// across reused containers.
async fn install_hll_extension(base_url: &str) {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(base_url)
        .await
        .expect("connect to bootstrap postgres failed");
    // `IF NOT EXISTS` is *not* atomic against the catalog: under
    // concurrent invocation (nextest spawns one test process at a
    // time per worker, each running this OnceCell initialiser), two
    // racers can both pass the IF-NOT-EXISTS check and then collide
    // on the `pg_extension_name_index` unique constraint (SQLSTATE
    // 23505). Treat that specific collision as success — the other
    // process won the race; the extension is installed.
    let res = sqlx::query("CREATE EXTENSION IF NOT EXISTS hll")
        .execute(&pool)
        .await;
    if let Err(e) = res {
        let already_present =
            e.as_database_error().and_then(|d| d.code()).as_deref() == Some("23505");
        if !already_present {
            panic!(
                "CREATE EXTENSION hll failed — image must ship the postgresql-hll extension: {e}"
            );
        }
    }
    pool.close().await;
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
