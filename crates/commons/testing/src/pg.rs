//! Test-only helper: provision a postgres `PgPool` for e2e tests.
//!
//! Two modes, chosen at runtime:
//!
//! 1. If `AIR_ELT_TEST_PG_URL` is set, connect there and create a unique
//!    sandbox schema per test. The schema is dropped when the handle is
//!    dropped. CI uses this mode (GHA `services.postgres`).
//! 2. Otherwise launch a fresh postgres container via `testcontainers`.
//!
//! Callers see the same `PgTestHandle` either way.

use rand::distr::Alphanumeric;
use rand::{Rng, rng};
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, ImageExt};
use testcontainers_modules::postgres::Postgres as PgImage;
use tracing::{info, warn};

/// Live handle to a postgres instance for a test. Drop → cleanup.
pub struct PgTestHandle {
    pub pool: PgPool,
    pub url: String,
    pub schema: String,
    _cleanup: CleanupGuard,
}

impl PgTestHandle {
    /// URL with `search_path` already bound to the sandbox schema.
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
                // Run cleanup on a fresh OS thread with its own current-thread runtime
                // — avoids deadlocks whether we're inside a live tokio runtime or not.
                let join = std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("build cleanup runtime");
                    rt.block_on(async move {
                        let stmt = format!("DROP SCHEMA IF EXISTS \"{schema}\" CASCADE");
                        if let Err(e) = sqlx::query(&stmt).execute(&pool).await {
                            warn!(error = %e, schema, "failed to drop test schema");
                        }
                    });
                });
                if let Err(e) = join.join() {
                    warn!(?e, "cleanup thread panicked");
                }
            }
            CleanupGuard::Container { .. } => {
                // testcontainers handles teardown on its own Drop.
            }
        }
    }
}

/// Entry point. Must be called from within a tokio runtime.
///
/// Fails fast with a human-readable panic message if the machine has no way
/// to reach a postgres (no `AIR_ELT_TEST_PG_URL`, no container runtime) — this
/// saves 30-120s of waiting on a bollard connect timeout.
pub async fn pg_pool() -> PgTestHandle {
    if let Ok(external) = std::env::var("AIR_ELT_TEST_PG_URL") {
        return external_with_sandbox(&external).await;
    }
    // Why spawn_blocking + 300ms timeout: unix socket probing uses sync
    // `UnixStream::connect` on a potentially-wedged docker.sock; without the
    // bound, a misbehaving socket can block a tokio worker for seconds.
    let backend = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        tokio::task::spawn_blocking(detect_backend),
    )
    .await
    .unwrap_or_else(|_| panic!("detect_backend timed out after 300ms"))
    .unwrap_or_else(|e| panic!("detect_backend task panicked: {e}"));
    match backend {
        Err(e) => panic!("{e}"),
        Ok(TestBackend::ExternalUrl) => unreachable!("handled above"),
        Ok(TestBackend::Container { socket }) => {
            if std::env::var_os("DOCKER_HOST").is_none() {
                // Why: testcontainers reads DOCKER_HOST on first call; we
                // export the auto-discovered socket before it runs so it
                // doesn't fall back to a stale /var/run/docker.sock and hang.
                // `set_var` is unsafe in edition 2024 due to the read-race
                // contract — we're still before any tokio worker starts using
                // the env variable, so the race window is closed.
                #[allow(unsafe_code)]
                unsafe {
                    std::env::set_var("DOCKER_HOST", &socket);
                }
            }
            spawn_container().await
        }
    }
}

/// Report whether a usable backend is available. `Ok(mode)` tells you what
/// the test will run against; `Err` is a human-readable description of why
/// nothing works — tests may call this directly to short-circuit early.
pub fn detect_backend() -> Result<TestBackend, BackendError> {
    if std::env::var("AIR_ELT_TEST_PG_URL").is_ok() {
        return Ok(TestBackend::ExternalUrl);
    }
    if let Some(path) = which_container_socket() {
        return Ok(TestBackend::Container { socket: path });
    }
    Err(BackendError::NoBackend)
}

#[derive(Debug, Clone)]
pub enum TestBackend {
    ExternalUrl,
    Container { socket: String },
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error(
        "no postgres backend for e2e tests.\n\n\
         Set one of:\n\
         - AIR_ELT_TEST_PG_URL=postgres://…  (CI / shared DB)\n\
         - DOCKER_HOST=unix:///path/to/docker-or-podman.sock  (testcontainers)\n\n\
         On macOS with podman, check `podman machine list` and point DOCKER_HOST at\n\
         the `-api.sock` file inside /var/folders/.../T/podman/, e.g.\n\
         DOCKER_HOST=unix:///var/folders/<uid>/T/podman/podman-machine-default-api.sock"
    )]
    NoBackend,
}

fn which_container_socket() -> Option<String> {
    if let Ok(host) = std::env::var("DOCKER_HOST")
        && host_is_alive(&host)
    {
        return Some(host);
    }
    // Default docker socket on Linux (also works for Docker Desktop on macOS).
    if socket_reachable("/var/run/docker.sock") {
        return Some("unix:///var/run/docker.sock".to_string());
    }
    // Rootless podman on Linux.
    if let Some(uid) = best_effort_uid() {
        let linux_podman = format!("/run/user/{uid}/podman/podman.sock");
        if socket_reachable(&linux_podman) {
            return Some(format!("unix://{linux_podman}"));
        }
    }
    // Podman on macOS — `podman machine` exposes the docker-API socket under
    // `$TMPDIR/podman/<machine>-api.sock`. We scan the standard location so
    // the user doesn't have to set DOCKER_HOST by hand.
    if let Some(path) = scan_macos_podman_sockets() {
        return Some(format!("unix://{path}"));
    }
    None
}

fn scan_macos_podman_sockets() -> Option<String> {
    let tmp = std::env::var("TMPDIR").ok()?;
    let dir = std::path::PathBuf::from(tmp).join("podman");
    let entries = std::fs::read_dir(&dir).ok()?;
    // Prefer the `-api.sock` file, that's the docker-compatible endpoint.
    let mut best: Option<String> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.ends_with("-api.sock") {
            continue;
        }
        let Some(path_str) = path.to_str() else {
            continue;
        };
        if socket_reachable(path_str) {
            best = Some(path_str.to_string());
            break;
        }
    }
    best
}

/// We need the POSIX uid only to build the rootless-podman socket path on Linux.
/// To avoid a `libc` dependency (and any `unsafe`), we read `$UID` — set by
/// every POSIX shell. Returning `None` just skips the rootless-podman probe;
/// the macOS branch and the explicit `DOCKER_HOST` fallback still apply.
fn best_effort_uid() -> Option<u32> {
    std::env::var("UID").ok().and_then(|s| s.parse().ok())
}

fn host_is_alive(host: &str) -> bool {
    if let Some(path) = host.strip_prefix("unix://") {
        socket_reachable(path)
    } else {
        // TCP / npipe — assume the user knows what they set.
        true
    }
}

fn socket_reachable(path: &str) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

async fn external_with_sandbox(url: &str) -> PgTestHandle {
    let schema = random_schema();
    info!(schema = %schema, "creating sandbox schema on external postgres");

    let bootstrap_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(url)
        .await
        .expect("connect to AIR_ELT_TEST_PG_URL failed");

    // Self-heal: previous test runs that crashed before Drop leave orphaned
    // `test_<unix_ts>_*` schemas. Every pg_pool() call cleans up any schema
    // older than 24h — cheap single-statement janitorial work that keeps
    // shared CI DBs from accumulating tens of thousands of stale schemas.
    drop_stale_test_schemas(&bootstrap_pool, 24 * 3600).await;

    let create = format!("CREATE SCHEMA \"{schema}\"");
    sqlx::query(&create)
        .execute(&bootstrap_pool)
        .await
        .expect("create sandbox schema failed");

    let scoped_url = format!("{url}?options=-c%20search_path%3D{schema}");
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&scoped_url)
        .await
        .expect("connect to sandbox schema failed");

    PgTestHandle {
        pool,
        url: url.to_string(),
        schema: schema.clone(),
        _cleanup: CleanupGuard::ExternalSchema {
            pool: bootstrap_pool,
            schema,
        },
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

    // Default credentials from testcontainers-modules Postgres.
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

/// Drop every `test_<unix_ts>_<suffix>` schema whose timestamp component is
/// older than `max_age_secs`. Failures are logged but do not abort the test —
/// the worst case is continued accumulation, not false failures.
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
        // schema_name is like `test_1700000000_abcd1234`; parse the middle part.
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
