//! Test-only: bootstrap a ryuk reaper sidecar and a session id shared across
//! every test process of the current `cargo test` / `cargo nextest run`.
//!
//! Why ryuk: testcontainers-rs leaves container cleanup to `Drop` and the
//! optional `watchdog` feature (signal handlers). Both fail under nextest's
//! process-per-test model — every test starts a fresh process, and the
//! short-lived statics that own containers vanish before any cleanup runs.
//! `testcontainers/ryuk` is the same sidecar Java/Go testcontainers ship: a
//! tiny daemon with the docker socket bind-mounted, accepting TCP filter
//! strings and forcibly removing matching containers when the last client
//! connection closes plus a grace period.
//!
//! Lifecycle:
//!
//! 1. The first test process to call [`ensure_session`] computes a session id
//!    (env override or per-cargo-target file under `temp_dir()`), takes a
//!    file lock, and starts the ryuk container with
//!    `ReuseDirective::Always`. testcontainers' reuse semantics ensure later
//!    callers see the running container instead of re-creating it.
//! 2. Every test process opens a TCP connection to ryuk and sends a label
//!    filter `label=air-elt.session=<sid>`. As long as one connection is
//!    open, ryuk preserves matching containers.
//! 3. The connection is held by a `static` for the lifetime of the test
//!    process; the OS closes it on exit (clean or abrupt). When all clients
//!    disconnect, ryuk waits `RYUK_RECONNECTION_TIMEOUT` (5 min) for new
//!    connections, then removes every matching container — including the
//!    user's pg / mongo / mysql infra — and finally exits, removing itself
//!    via the `--rm` (auto_remove) flag.
//!
//! Cross-platform: this module relies only on `std::fs::File::lock` (stable
//! since Rust 1.89), `std::env::temp_dir`, and `std::net::TcpStream`. No
//! `libc` / `winapi` extern blocks.

use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use testcontainers::core::{ContainerPort, Mount};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, GenericImage, ImageExt, ReuseDirective};
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

/// Label key applied to every container we want ryuk to clean up. The value
/// is `session_id()`, identical for all test processes of one cargo
/// invocation.
pub const SESSION_LABEL_KEY: &str = "air-elt.session";

/// Override env-var: when set, replaces the auto-derived session id. CI
/// pipelines should set this to the job id so parallel jobs on shared
/// infrastructure don't share a ryuk.
const SESSION_ID_ENV: &str = "AIR_ELT_TEST_SESSION_ID";

const RYUK_IMAGE: &str = "testcontainers/ryuk";
const RYUK_TAG: &str = "0.11.0";
const RYUK_PORT: u16 = 8080;

/// File-based session id staleness window. A session-id file older than this
/// (and whose ryuk container is no longer running) is rewritten on next
/// start. 6 h is comfortably longer than any realistic test run.
const SESSION_FILE_TTL_SECS: u64 = 6 * 3600;

/// How long we wait for ryuk to start listening before giving up.
/// Local container start is bounded by ~3-5 s on healthy systems; 10 s
/// is plenty without dragging real failures out.
const RYUK_TCP_DEADLINE: Duration = Duration::from_secs(10);

static SESSION_ID: OnceLock<String> = OnceLock::new();
static SESSION_HANDLE: OnceCell<SessionHandle> = OnceCell::const_new();

#[allow(dead_code)]
struct SessionHandle {
    /// Held open for the lifetime of the test process. Dropping it ends the
    /// ryuk client session for this process, which is exactly what we want
    /// on process exit — ryuk waits for the rest of the cohort to drop too,
    /// then reaps everything.
    keepalive: TcpStream,
    /// Owning handle to the ryuk container itself. We never drop it; the
    /// container is auto-removed when ryuk exits after all clients leave.
    _ryuk: ContainerAsync<GenericImage>,
}

/// Process-stable session id. First call computes it under a file lock so
/// multiple test processes started by the same cargo invocation pick up the
/// same value.
pub fn session_id() -> &'static str {
    SESSION_ID.get_or_init(compute_session_id)
}

/// Convenience: the `(key, value)` pair to pass to
/// `ImageExt::with_label(...)` on every infra container so ryuk can match
/// it for cleanup.
pub fn session_label() -> (&'static str, &'static str) {
    (SESSION_LABEL_KEY, session_id())
}

/// Idempotently bring up a ryuk sidecar for the current session and register
/// this process as a client. Safe to call from many tests in parallel.
///
/// `socket` is the `DOCKER_HOST`-style endpoint detected by `backend.rs`
/// (e.g. `unix:///var/run/docker.sock`). It's the path we bind-mount into
/// ryuk so the daemon can talk to docker.
pub async fn ensure_session(socket: &str) {
    let socket = socket.to_string();
    SESSION_HANDLE
        .get_or_init(|| async move {
            // The cross-process lock is now scoped narrowly inside
            // `start_session` — it covers only the create-or-reuse race
            // window (`image.start().await`), so the TCP handshake and
            // ACK exchange run unlocked. Force-removes between retries
            // are also taken under the same lock to avoid stomping on
            // a sibling process that just won the start race.
            let sid = session_id();
            let container_name = format!("air-elt-ryuk-{}", sanitize_for_container_name(sid));
            let mut last_err = String::new();
            for attempt in 0..3 {
                if attempt > 0 {
                    let _lock = crate::filelock::acquire_lock("ryuk");
                    if let Err(e) = remove_ryuk_container(&container_name).await {
                        warn!(error = %e, "could not force-remove ryuk before retry");
                    }
                    // Brief settle so docker has time to actually evict
                    // the container before testcontainers re-lists by
                    // name. 150 ms is plenty for the daemon's internal
                    // bookkeeping; longer waits just inflate retry cost.
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                match start_session(&socket).await {
                    Ok(handle) => return handle,
                    Err(e) => {
                        warn!(attempt, error = %e, "ryuk session handshake failed; retrying");
                        last_err = e;
                    }
                }
            }
            panic!("failed to bring up ryuk session after 3 attempts: {last_err}");
        })
        .await;
}

/// Best-effort `DELETE /containers/<name>?force=true` via bollard. Used
/// to evict a broken ryuk between handshake retries — testcontainers'
/// reuse-mode would otherwise hand us back the same dud. Operates on
/// the same `DOCKER_HOST` we exported from `backend.rs`, so behaviour
/// matches what testcontainers itself sees.
async fn remove_ryuk_container(name: &str) -> Result<(), bollard::errors::Error> {
    let docker = bollard::Docker::connect_with_local_defaults()?;
    let opts = bollard::query_parameters::RemoveContainerOptionsBuilder::default()
        .force(true)
        .build();
    docker.remove_container(name, Some(opts)).await
}

async fn start_session(socket: &str) -> Result<SessionHandle, String> {
    let sid = session_id();
    let container_name = format!("air-elt-ryuk-{}", sanitize_for_container_name(sid));

    // `socket` is logged for diagnostics only; the bind-mount uses the
    // canonical in-container path (see comment on `with_mount` below).
    info!(
        sid = %sid,
        ryuk_image = %format!("{RYUK_IMAGE}:{RYUK_TAG}"),
        host_socket = %socket,
        "ensuring ryuk session"
    );
    let image = GenericImage::new(RYUK_IMAGE, RYUK_TAG)
        .with_exposed_port(ContainerPort::Tcp(RYUK_PORT))
        .with_container_name(container_name)
        .with_label(SESSION_LABEL_KEY, sid)
        // Reconnection grace must cover the gap between sibling test
        // binaries: `cargo test --workspace` runs each binary in its own
        // process. The drop chain on exit (tokio runtime, sqlx pools,
        // bollard) plus the next binary's bollard/testcontainers init
        // can stretch the gap well past 30 s in practice — observed
        // containers cycling 5× in 2 min. 5 min is comfortably wider
        // than any realistic inter-binary gap and only delays final
        // teardown after the last test exits.
        .with_env_var("RYUK_RECONNECTION_TIMEOUT", "10m")
        // Initial-connection timeout: if nothing dials ryuk within this,
        // it self-shuts. 120 s accommodates slow MSSQL image pulls.
        .with_env_var("RYUK_CONNECTION_TIMEOUT", "120s")
        .with_env_var("RYUK_VERBOSE", "true")
        // Inside-container path of the docker-compat socket. macOS/podman
        // and Docker Desktop both expose `/var/run/docker.sock` to
        // containers regardless of where the host-side endpoint lives, so
        // we always bind-mount the canonical path. The host-side
        // `socket` we detected via `backend.rs` is only used by
        // bollard/testcontainers from the host; ryuk talks to the daemon
        // from *inside* a container.
        .with_mount(Mount::bind_mount(
            "/var/run/docker.sock",
            "/var/run/docker.sock",
        ))
        // The official testcontainers/ryuk image runs as user 65532 (nonroot)
        // by default. On rootless podman the socket is mode 0660 and not
        // readable by that user, so we override to root. Privileged mode
        // also relaxes any SELinux/AppArmor labels around the bind mount.
        .with_user("0:0")
        .with_privileged(true)
        .with_reuse(ReuseDirective::Always);
    // Lock only across the create-or-reuse race window — TCP handshake
    // and the ACK exchange below are safe under concurrency.
    let start_lock = crate::filelock::acquire_lock("ryuk");
    let container = image
        .start()
        .await
        .map_err(|e| format!("start ryuk container failed: {e}"))?;
    drop(start_lock);
    let host = container
        .get_host()
        .await
        .map_err(|e| format!("ryuk get_host failed: {e}"))?
        .to_string();
    let port = container
        .get_host_port_ipv4(RYUK_PORT)
        .await
        .map_err(|e| format!("ryuk get_host_port_ipv4 failed: {e}"))?;

    let mut stream = wait_for_ryuk_listener(&host, port).await?;
    let filter = format!("label={SESSION_LABEL_KEY}={sid}\n");
    stream
        .write_all(filter.as_bytes())
        .map_err(|e| format!("write ryuk filter: {e}"))?;
    // Ryuk replies with "ACK\n" per match; we sent one filter so one ACK.
    let mut buf = [0u8; 4];
    // Local ACK comes back inside a single TCP RTT — give it 1 s and
    // let the retry path handle anything slower (typically means ryuk
    // is in shutdown).
    stream
        .set_read_timeout(Some(Duration::from_secs(1)))
        .map_err(|e| format!("set ryuk read timeout: {e}"))?;
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("read ryuk ACK: {e}"))?;
    if &buf != b"ACK\n" {
        return Err(format!(
            "unexpected ryuk handshake reply: {:?}",
            String::from_utf8_lossy(&buf)
        ));
    }
    // Clear the read timeout — we don't read again, but we want the kernel
    // to keep the socket open for the rest of the process lifetime.
    stream
        .set_read_timeout(None)
        .map_err(|e| format!("clear ryuk read timeout: {e}"))?;
    debug!(sid = %sid, "ryuk session ACKed");
    Ok(SessionHandle {
        keepalive: stream,
        _ryuk: container,
    })
}

async fn wait_for_ryuk_listener(host: &str, port: u16) -> Result<TcpStream, String> {
    let deadline = Instant::now() + RYUK_TCP_DEADLINE;
    let addr = format!("{host}:{port}");
    let resolved = addr
        .parse()
        .or_else(|_| addr.to_socket_first())
        .map_err(|e| format!("parse ryuk addr {addr}: {e}"))?;
    // Locally ryuk binds within ~tens of ms once the container reports
    // running. We poll fast (100 ms cadence, 200 ms connect timeout)
    // because even on a cold machine the listener is up well within a
    // second; longer waits just inflate test latency.
    let mut last_err: Option<std::io::Error> = None;
    while Instant::now() < deadline {
        match TcpStream::connect_timeout(&resolved, Duration::from_millis(200)) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }
    Err(format!(
        "ryuk did not start listening on {addr} within {:?}: {:?}",
        RYUK_TCP_DEADLINE, last_err
    ))
}

trait ToSocketFirst {
    fn to_socket_first(&self) -> Result<std::net::SocketAddr, String>;
}

impl ToSocketFirst for str {
    fn to_socket_first(&self) -> Result<std::net::SocketAddr, String> {
        use std::net::ToSocketAddrs;
        self.to_socket_addrs()
            .map_err(|e| e.to_string())?
            .next()
            .ok_or_else(|| format!("no socket address resolved for {self}"))
    }
}

fn sanitize_for_container_name(sid: &str) -> String {
    // Docker container names must match `[a-zA-Z0-9][a-zA-Z0-9_.-]+`. Our
    // generated ids are `<pid>-<unix_ts>` so already fine, but env-overrides
    // may bring in arbitrary characters.
    sid.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn compute_session_id() -> String {
    if let Ok(env) = std::env::var(SESSION_ID_ENV) {
        return env.trim().to_string();
    }
    match read_or_mint_session_file() {
        Ok(id) => id,
        Err(e) => {
            warn!(error = %e, "session id file unavailable, falling back to per-process id");
            mint_id()
        }
    }
}

fn read_or_mint_session_file() -> Result<String, String> {
    let dir = std::env::temp_dir().join("air-elt-test-session");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let target_hash = target_hash();
    let lock_path = dir.join(format!("{target_hash}.lock"));
    let id_path = dir.join(format!("{target_hash}.id"));

    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("open lock {}: {e}", lock_path.display()))?;
    // Stable cross-platform `File::lock` since Rust 1.89.
    lock_file
        .lock()
        .map_err(|e| format!("acquire lock {}: {e}", lock_path.display()))?;
    let id = read_or_mint_under_lock(&id_path)?;
    // Lock dropped when `lock_file` falls out of scope.
    drop(lock_file);
    Ok(id)
}

fn read_or_mint_under_lock(id_path: &PathBuf) -> Result<String, String> {
    if id_path.exists() {
        let raw = std::fs::read_to_string(id_path)
            .map_err(|e| format!("read {}: {e}", id_path.display()))?;
        if !is_id_stale(&raw) {
            return Ok(raw.trim().to_string());
        }
    }
    let new = mint_id();
    std::fs::write(id_path, &new).map_err(|e| format!("write {}: {e}", id_path.display()))?;
    Ok(new)
}

fn mint_id() -> String {
    let pid = std::process::id();
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{pid}-{ts}")
}

fn is_id_stale(raw: &str) -> bool {
    let raw = raw.trim();
    let Some((_, ts)) = raw.split_once('-') else {
        return true;
    };
    let Ok(ts) = ts.parse::<u64>() else {
        return true;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now.saturating_sub(ts) > SESSION_FILE_TTL_SECS
}

fn target_hash() -> String {
    // Per-workspace key. We must NOT use CARGO_MANIFEST_DIR — cargo sets it
    // per-crate, so two test binaries from sibling crates would compute
    // different hashes and end up in different ryuk sessions, defeating
    // sharing. Workspace root is the dir containing `target/`, which we can
    // locate by walking up from the running test binary path.
    let key = std::env::var("CARGO_TARGET_DIR")
        .ok()
        .or_else(workspace_root_from_current_exe)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.display().to_string())
        })
        .unwrap_or_default();
    let hash = Sha256::digest(key.as_bytes());
    hash.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Walk up from the running test binary path looking for a `target`
/// ancestor; return its parent (the workspace root). All test binaries of
/// one cargo invocation share the same `target/` so hashing this path
/// gives every test process the same session id.
fn workspace_root_from_current_exe() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let mut cursor = exe.as_path();
    while let Some(parent) = cursor.parent() {
        if parent.file_name().and_then(|n| n.to_str()) == Some("target") {
            return parent.parent().map(|p| p.display().to_string());
        }
        cursor = parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stale_for_old_timestamp() {
        let stale = format!("123-{}", 1);
        assert!(is_id_stale(&stale));
    }

    #[test]
    fn id_is_fresh_for_now() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_secs();
        let fresh = format!("123-{now}");
        assert!(!is_id_stale(&fresh));
    }

    #[test]
    fn id_is_stale_when_unparsable() {
        assert!(is_id_stale("garbage"));
        assert!(is_id_stale(""));
    }

    #[test]
    fn target_hash_stable_for_same_input() {
        // Same env → same hash on repeated calls in the same process.
        let a = target_hash();
        let b = target_hash();
        assert_eq!(a, b);
        assert_eq!(a.len(), 16); // 8 bytes hex
    }

    #[test]
    fn sanitize_replaces_disallowed_chars() {
        assert_eq!(sanitize_for_container_name("abc-123_4"), "abc-123_4");
        assert_eq!(sanitize_for_container_name("a/b c"), "a_b_c");
    }
}
