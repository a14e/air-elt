//! Sandboxed Valkey (redis-compatible) handle for tests.
//!
//! Two modes:
//!
//! 1. If `AIR_ELT_TEST_VALKEY_URL` is set, connect directly. CI uses this
//!    mode.
//! 2. Otherwise launch a fresh Valkey container via testcontainers using
//!    the pinned image `mirror.gcr.io/valkey/valkey:8.1.1`. The container is labelled
//!    with the current ryuk session so it's shared across every test
//!    process of one cargo invocation and reaped automatically.
//!
//! Per-test isolation is by a unique random key prefix (see
//! [`ValkeyTestHandle::key`]) — the shared container is reaped at session
//! end, so there is no cross-test cleanup to perform.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt, ReuseDirective};
use tokio::sync::OnceCell;
use tracing::info;

use crate::backend::{TestBackend, detect_with_timeout, prepare_container_env};
use crate::ryuk;

/// Valkey image tag — single source of truth.
/// Must match `.github/workflows/ci.yml`'s docker run for the valkey
/// service. A CI step grep-asserts this exact string appears in the
/// workflow file so drift between the two surfaces fails fast.
pub const VALKEY_IMAGE_TAG: &str = "mirror.gcr.io/valkey/valkey:8.1.1";

const URL_VAR: &str = "AIR_ELT_TEST_VALKEY_URL";

const KIND_LABEL_KEY: &str = "air-elt.kind";
const KIND_LABEL_VALUE: &str = "valkey";

const VALKEY_PORT: u16 = 6379;

const READINESS_DEADLINE: Duration = Duration::from_secs(60);
const READINESS_POLL_INTERVAL: Duration = Duration::from_millis(250);

static VALKEY_CONTAINER: OnceCell<Arc<ContainerAsync<GenericImage>>> = OnceCell::const_new();

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Sandbox handle for Valkey tests.
pub struct ValkeyTestHandle {
    /// Connection URL (`redis://<host>:<port>`).
    pub url: String,
    /// Unique key prefix for this handle, so concurrent tests sharing the
    /// container don't collide on keys.
    pub key_prefix: String,
    /// Hold the container so it lives until handle drop. `None` when the
    /// caller pointed us at an externally-managed Valkey via env vars.
    _container: Option<Arc<ContainerAsync<GenericImage>>>,
}

impl ValkeyTestHandle {
    /// Namespace a key under this handle's unique prefix.
    pub fn key(&self, suffix: &str) -> String {
        format!("{}{}", self.key_prefix, suffix)
    }
}

/// Build a fresh `ValkeyTestHandle`. The underlying container (when not
/// externally provided) is reused across calls via `ReuseDirective::Always`.
pub fn valkey_handle()
-> Pin<Box<dyn Future<Output = Result<ValkeyTestHandle, BoxError>> + Send + 'static>> {
    Box::pin(async move { valkey_handle_impl().await })
}

async fn valkey_handle_impl() -> Result<ValkeyTestHandle, BoxError> {
    let key_prefix = unique_prefix();

    if let Ok(url) = std::env::var(URL_VAR) {
        info!("using externally-provided Valkey endpoint");
        wait_for_ready(&url).await?;
        return Ok(ValkeyTestHandle {
            url,
            key_prefix,
            _container: None,
        });
    }

    let container = ensure_container().await?;
    let host = container.get_host().await?.to_string();
    let port = container.get_host_port_ipv4(VALKEY_PORT).await?;
    info!(host = %host, port, "discovered Valkey host port");

    let url = format!("redis://{host}:{port}");
    wait_for_ready(&url).await?;

    Ok(ValkeyTestHandle {
        url,
        key_prefix,
        _container: Some(container),
    })
}

fn unique_prefix() -> String {
    let token: u64 = rand::random();
    format!("airelt:test:{token:016x}:")
}

async fn ensure_container() -> Result<Arc<ContainerAsync<GenericImage>>, BoxError> {
    let arc = VALKEY_CONTAINER.get_or_try_init(start_container).await?;
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
        image = %VALKEY_IMAGE_TAG,
        "ensuring shared valkey container (reuse=Always, ryuk-managed)"
    );

    let (image_repo, image_tag) = VALKEY_IMAGE_TAG
        .split_once(':')
        .ok_or_else(|| -> BoxError {
            format!("VALKEY_IMAGE_TAG missing ':': {VALKEY_IMAGE_TAG}").into()
        })?;
    let image = GenericImage::new(image_repo, image_tag)
        .with_exposed_port(ContainerPort::Tcp(VALKEY_PORT))
        // valkey logs `Ready to accept connections tcp` once the listener
        // is bound; the readiness probe then confirms PING.
        .with_wait_for(WaitFor::message_on_stdout("Ready to accept connections"))
        .with_container_name(format!("air-elt-valkey-tc-{session_value}"))
        .with_label(KIND_LABEL_KEY, KIND_LABEL_VALUE)
        .with_label(session_key, session_value)
        .with_reuse(ReuseDirective::Always);

    // Lock only across the create-or-reuse race window. The readiness
    // probe runs unlocked so sibling processes proceed in parallel.
    let start_lock = crate::filelock::acquire_lock("valkey-tc");
    let container = image.start().await?;
    drop(start_lock);
    info!("valkey container started");
    Ok(Arc::new(container))
}

/// Poll `PING` against the endpoint until it answers, so a test query
/// never races the listener bind.
async fn wait_for_ready(url: &str) -> Result<(), BoxError> {
    let client = redis::Client::open(url)?;
    let deadline = std::time::Instant::now() + READINESS_DEADLINE;
    let mut last_error: Option<String> = None;
    while std::time::Instant::now() < deadline {
        match client.get_multiplexed_async_connection().await {
            Ok(mut conn) => match redis::cmd("PING").query_async::<String>(&mut conn).await {
                Ok(_) => {
                    info!("valkey ready");
                    return Ok(());
                }
                Err(error) => last_error = Some(error.to_string()),
            },
            Err(error) => last_error = Some(error.to_string()),
        }
        tokio::time::sleep(READINESS_POLL_INTERVAL).await;
    }
    Err(format!("valkey did not answer PING within {READINESS_DEADLINE:?}: {last_error:?}").into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_tag_pinned() {
        assert!(
            VALKEY_IMAGE_TAG.ends_with("valkey/valkey:8.1.1"),
            "Valkey image tag must end with valkey/valkey:8.1.1, got: {VALKEY_IMAGE_TAG}"
        );
    }

    #[test]
    fn unique_prefix_differs() {
        assert_ne!(unique_prefix(), unique_prefix());
    }
}
