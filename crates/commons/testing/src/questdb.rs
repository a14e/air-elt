//! Sandboxed QuestDB handle for tests.
//!
//! Two modes:
//!
//! 1. If `AIR_ELT_TEST_QUESTDB_URL` is set, connect directly. CI uses
//!    this mode.
//! 2. Otherwise launch a fresh QuestDB container via testcontainers using
//!    the pinned image `questdb/questdb:8.2.3`. The container is labelled
//!    with the current ryuk session so it's shared across every test
//!    process of one cargo invocation and reaped automatically.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testcontainers::core::{ContainerPort, Mount, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt, ReuseDirective};
use tokio::sync::OnceCell;
use tracing::{info, warn};

use air_elt_commons_questdb::identifier::quote_ident;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

/// QuestDB image tag — single source of truth.
/// Must match `.github/workflows/ci.yml`'s docker run for the questdb
/// service. A CI step grep-asserts this exact string appears in the
/// workflow file so drift between the two surfaces fails fast.
pub const QUESTDB_IMAGE_TAG: &str = "mirror.gcr.io/questdb/questdb:8.2.3";

const URL_VAR: &str = "AIR_ELT_TEST_QUESTDB_URL";

const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "questdb";

const QUESTDB_PG_PORT: u16 = 8812;

/// QuestDB default superuser / password / database — fixed by the image
/// (no env knobs in 8.2.3 community).
const QUESTDB_USER: &str = "admin";
const QUESTDB_PASSWORD: &str = "quest";
const QUESTDB_DATABASE: &str = "qdb";

/// pg-wire readiness probe deadline.
const READINESS_DEADLINE: Duration = Duration::from_secs(60);
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Single shared container handle for the test session. The container
/// itself is also kept alive via the ryuk session label, so dropping
/// this Arc would not actually evict it — caching it just avoids
/// rebuilding the image / port lookup on every handle.
static QUESTDB_CONTAINER: OnceCell<Arc<ContainerAsync<GenericImage>>> = OnceCell::const_new();

/// Sandbox handle for QuestDB tests. The pg-wire `pool` is the only
/// transport — QuestDB exposes ingest over the same Postgres-wire
/// protocol on port 8812.
pub struct QuestDbTestHandle {
    /// pg-wire pool (`postgres://admin:quest@<host>:<port>/qdb`).
    pub pool: PgPool,
    /// pg-wire URL string.
    pub url: String,
    /// Hold the container so it lives until handle drop. `None` when the
    /// caller pointed us at an externally-managed QuestDB via env vars.
    _container: Option<Arc<ContainerAsync<GenericImage>>>,
}

impl QuestDbTestHandle {
    /// Execute a SQL statement against the pg-wire control pool.
    pub async fn exec(&self, sql: &str) -> Result<(), sqlx::Error> {
        sqlx::query(sql).execute(&self.pool).await?;
        Ok(())
    }

    /// Best-effort `DROP TABLE IF EXISTS "<table>"`. Logs a warning on
    /// failure but does not propagate — used in sandbox teardown where
    /// the next test creates a fresh table anyway.
    pub async fn drop_table(&self, table: &str) {
        let quoted = quote_ident(table);
        let sql = format!("DROP TABLE IF EXISTS {quoted}");
        if let Err(error) = sqlx::query(&sql).execute(&self.pool).await {
            warn!(table = %table, %error, "drop_table: best-effort DROP failed");
        }
    }
}

impl Drop for QuestDbTestHandle {
    fn drop(&mut self) {
        // No explicit teardown: the Arc<ContainerAsync> manages lifetime
        // alongside ryuk. Per project rule, tests must `pool.close().await`
        // themselves before exit to avoid the sqlx Drop hang.
    }
}

/// Build a fresh `QuestDbTestHandle`. The underlying container (when not
/// externally provided) is reused across calls via `ReuseDirective::Always`.
pub fn questdb_pool()
-> Pin<Box<dyn Future<Output = Result<QuestDbTestHandle, BoxError>> + Send + 'static>> {
    Box::pin(async move { questdb_pool_impl().await })
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn questdb_pool_impl() -> Result<QuestDbTestHandle, BoxError> {
    if let Ok(url) = std::env::var(URL_VAR) {
        info!(
            url = %redact_url(&url),
            "using externally-provided QuestDB endpoint"
        );
        let pool = build_pg_pool(&url).await?;
        return Ok(QuestDbTestHandle {
            pool,
            url,
            _container: None,
        });
    }

    let container = ensure_container().await?;
    let host = container.get_host().await?.to_string();
    let pg_port = container.get_host_port_ipv4(QUESTDB_PG_PORT).await?;
    info!(
        host = %host,
        pg_port,
        "discovered QuestDB host port"
    );

    let url =
        format!("postgres://{QUESTDB_USER}:{QUESTDB_PASSWORD}@{host}:{pg_port}/{QUESTDB_DATABASE}");
    wait_for_pg_ready(&url).await?;
    let pool = build_pg_pool(&url).await?;

    Ok(QuestDbTestHandle {
        pool,
        url,
        _container: Some(container),
    })
}

async fn build_pg_pool(url: &str) -> Result<PgPool, BoxError> {
    let pool = PgPoolOptions::new()
        .max_connections(4)
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_secs(60))
        .connect(url)
        .await?;
    Ok(pool)
}

async fn ensure_container() -> Result<Arc<ContainerAsync<GenericImage>>, BoxError> {
    let arc = QUESTDB_CONTAINER.get_or_try_init(start_container).await?;
    Ok(arc.clone())
}

async fn start_container() -> Result<Arc<ContainerAsync<GenericImage>>, BoxError> {
    let backend = detect_with_timeout(URL_VAR)
        .await
        .map_err(|e| -> BoxError { e.into() })?;
    let socket = match backend {
        TestBackend::ExternalUrl => {
            unreachable!("external URL path handled in caller")
        }
        TestBackend::Container { socket } => socket,
    };
    prepare_container_env(&socket);
    ryuk::ensure_session(&socket).await;
    let (session_key, session_value) = ryuk::session_label();
    info!(
        image = %QUESTDB_IMAGE_TAG,
        "ensuring shared questdb container (reuse=Always, ryuk-managed)"
    );

    let (image_repo, image_tag) =
        QUESTDB_IMAGE_TAG
            .split_once(':')
            .ok_or_else(|| -> BoxError {
                format!("QUESTDB_IMAGE_TAG missing ':': {QUESTDB_IMAGE_TAG}").into()
            })?;
    let image = GenericImage::new(image_repo, image_tag)
        .with_exposed_port(ContainerPort::Tcp(QUESTDB_PG_PORT))
        // QuestDB 8.2.3 logs the pg-wire listener readiness as
        // `A pg-server listening on 0.0.0.0:8812 ...`. Earlier images
        // used `Server is ready`; 8.2.3 drops that line entirely.
        // Match the listener log — it fires once the pg port is bound
        // and accepting connections, which is exactly what
        // `wait_for_pg_ready` then probes.
        .with_wait_for(WaitFor::message_on_stdout("pg-server listening on"))
        .with_container_name(format!("air-elt-questdb-tc-{session_value}"))
        .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
        .with_label(session_key, session_value)
        // tmpfs on the data dir: container is reaped at session end, so
        // on-disk state has no value. 512 MiB is more than enough for any
        // single test run.
        .with_mount(Mount::tmpfs_mount("/var/lib/questdb"))
        .with_reuse(ReuseDirective::Always);

    // Lock only across the create-or-reuse race window. The pg-wire
    // readiness probe and pool build run unlocked so sibling processes
    // can proceed in parallel.
    let start_lock = crate::filelock::acquire_lock("questdb-tc");
    let container = image.start().await?;
    drop(start_lock);
    info!("questdb container started");
    Ok(Arc::new(container))
}

/// Poll `SELECT 1` against the pg-wire endpoint until it succeeds.
/// QuestDB's `pg-server listening on …` log line lands the moment the
/// port is bound, but the planner may still be warming up — the extra
/// probe avoids races against early-arriving test queries.
async fn wait_for_pg_ready(url: &str) -> Result<(), BoxError> {
    let deadline = std::time::Instant::now() + READINESS_DEADLINE;
    let mut last_error: Option<String> = None;
    while std::time::Instant::now() < deadline {
        let attempt = PgPoolOptions::new()
            .max_connections(1)
            .acquire_timeout(Duration::from_secs(2))
            .connect(url)
            .await;
        match attempt {
            Ok(pool) => match sqlx::query("SELECT 1").execute(&pool).await {
                Ok(_) => {
                    pool.close().await;
                    info!("questdb pg-wire ready");
                    return Ok(());
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    pool.close().await;
                }
            },
            Err(error) => {
                last_error = Some(error.to_string());
            }
        }
        tokio::time::sleep(READINESS_POLL_INTERVAL).await;
    }
    Err(format!(
        "questdb pg-wire did not accept SELECT 1 within {READINESS_DEADLINE:?}: {last_error:?}"
    )
    .into())
}

/// Redact the password segment of a `postgres://user:pass@host` URL so
/// it's safe to emit at info level.
fn redact_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let body_start = scheme_end + 3;
    let body = &url[body_start..];
    let Some(at) = body.find('@') else {
        return url.to_string();
    };
    let auth = &body[..at];
    let Some(colon) = auth.find(':') else {
        return url.to_string();
    };
    let user = &auth[..colon];
    let rest = &body[at..];
    format!("{}://{}:***{}", &url[..scheme_end], user, rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_url_masks_password() {
        let redacted = redact_url("postgres://admin:quest@localhost:8812/qdb");
        assert_eq!(redacted, "postgres://admin:***@localhost:8812/qdb");
    }

    #[test]
    fn redact_url_passthrough_when_no_auth() {
        let raw = "postgres://localhost:8812/qdb";
        assert_eq!(redact_url(raw), raw);
    }

    // Pinned to 8.2.3: earlier versions (notably 8.1.1) mis-type extended-protocol
    // bind parameters as STRING for every non-STRING/non-LONG column, causing
    // validate_access dry-run probes to fail with "inconvertible types".
    #[test]
    fn image_tag_pinned() {
        assert!(
            QUESTDB_IMAGE_TAG.ends_with("questdb/questdb:8.2.3"),
            "QuestDB image tag must end with questdb/questdb:8.2.3, got: {QUESTDB_IMAGE_TAG}"
        );
    }
}
