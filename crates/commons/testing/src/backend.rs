//! Container-runtime detection shared between every per-db test helper.
//!
//! testcontainers reads `DOCKER_HOST` on first call. If we don't probe and
//! export it before that call, it falls back to a stale default and hangs
//! for tens of seconds. This module probes the standard locations
//! (docker, rootless podman, macOS podman, Docker Desktop named pipe on
//! Windows) up front, returns whatever endpoint actually accepts a
//! connection, and lets the caller export it.
//!
//! `prepare_container_env(socket)` exports the resolved endpoint as
//! `DOCKER_HOST` so testcontainers picks it up. Container lifetime is
//! managed by the ryuk sidecar (see `ryuk.rs`), not by signal handlers or
//! `atexit` hooks — every container we start carries an
//! `air-elt.session=<id>` label, and ryuk reaps them when the last test
//! process disconnects.

#[derive(Debug, Clone)]
pub enum TestBackend {
    ExternalUrl,
    Container { socket: String },
}

#[derive(Debug, thiserror::Error)]
#[error(
    "no container backend for e2e tests.\n\n\
     Set one of:\n\
     - <db>-specific URL env var (see `pg_pool` / `mysql_pool` docs)\n\
     - DOCKER_HOST=unix:///path/to/docker-or-podman.sock  (testcontainers, POSIX)\n\
     - DOCKER_HOST=npipe:////./pipe/docker_engine  (Docker Desktop on Windows)\n\n\
     On macOS with podman, check `podman machine list` and point DOCKER_HOST at\n\
     the `-api.sock` file inside /var/folders/.../T/podman/, e.g.\n\
     DOCKER_HOST=unix:///var/folders/<uid>/T/podman/podman-machine-default-api.sock"
)]
pub struct NoBackend;

/// Detect a container socket (or signal that an external URL is set up).
/// `external_url_var` lets each db helper announce its own override env var
/// in the panic message.
pub fn detect(external_url_var: &str) -> Result<TestBackend, NoBackend> {
    if std::env::var(external_url_var).is_ok() {
        return Ok(TestBackend::ExternalUrl);
    }
    if let Some(path) = which_container_endpoint() {
        return Ok(TestBackend::Container { socket: path });
    }
    Err(NoBackend)
}

/// Export the container env vars before testcontainers runs.
pub fn prepare_container_env(socket: &str) {
    #[allow(unsafe_code)]
    unsafe {
        // Why: testcontainers reads DOCKER_HOST on first call; we export the
        // auto-discovered endpoint before it runs so it doesn't fall back to
        // a stale default and hang. Container cleanup is handled by ryuk
        // (see `ryuk.rs`), so no further env is required here.
        if std::env::var_os("DOCKER_HOST").is_none() {
            std::env::set_var("DOCKER_HOST", socket);
        }
    }
}

#[cfg(unix)]
fn which_container_endpoint() -> Option<String> {
    if let Ok(host) = std::env::var("DOCKER_HOST")
        && host_is_alive(&host)
    {
        return Some(host);
    }
    if endpoint_reachable("/var/run/docker.sock") {
        return Some("unix:///var/run/docker.sock".to_string());
    }
    if let Some(uid) = best_effort_uid() {
        let linux_podman = format!("/run/user/{uid}/podman/podman.sock");
        if endpoint_reachable(&linux_podman) {
            return Some(format!("unix://{linux_podman}"));
        }
    }
    if let Some(path) = scan_macos_podman_sockets() {
        return Some(format!("unix://{path}"));
    }
    None
}

#[cfg(windows)]
fn which_container_endpoint() -> Option<String> {
    if let Ok(host) = std::env::var("DOCKER_HOST")
        && host_is_alive(&host)
    {
        return Some(host);
    }
    // Docker Desktop on Windows. Both pipes are aliases for the same daemon
    // depending on engine choice; `docker_engine` is the default, the
    // `dockerDesktopLinuxEngine` variant is exposed when the Linux-engine
    // toggle is on.
    for pipe in ["docker_engine", "dockerDesktopLinuxEngine"] {
        let path = format!(r"\\.\pipe\{pipe}");
        if endpoint_reachable(&path) {
            return Some(format!("npipe:////./pipe/{pipe}"));
        }
    }
    None
}

#[cfg(unix)]
fn scan_macos_podman_sockets() -> Option<String> {
    let tmp = std::env::var("TMPDIR").ok()?;
    let dir = std::path::PathBuf::from(tmp).join("podman");
    let entries = std::fs::read_dir(&dir).ok()?;
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
        if endpoint_reachable(path_str) {
            best = Some(path_str.to_string());
            break;
        }
    }
    best
}

#[cfg(unix)]
fn best_effort_uid() -> Option<u32> {
    std::env::var("UID").ok().and_then(|s| s.parse().ok())
}

fn host_is_alive(host: &str) -> bool {
    if let Some(path) = host.strip_prefix("unix://") {
        endpoint_reachable(path)
    } else if let Some(rest) = host.strip_prefix("npipe://") {
        // `npipe:////./pipe/docker_engine` → `\\.\pipe\docker_engine`.
        // Tolerate either form by translating to the canonical Windows path.
        let pipe_path = rest.trim_start_matches('/').replace('/', r"\");
        endpoint_reachable(&format!(r"\\{pipe_path}"))
    } else {
        // tcp://host:port etc — assume the operator knows what they're doing.
        true
    }
}

#[cfg(unix)]
fn endpoint_reachable(path: &str) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

#[cfg(windows)]
fn endpoint_reachable(path: &str) -> bool {
    // Named pipes can be opened with `OpenOptions` once the server is
    // listening. We open in read+write to mirror how testcontainers (via
    // bollard) talks to the daemon, then drop immediately.
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .is_ok()
}

/// Synchronous wrapper that times out the detection probe — handy for
/// callers running inside a tokio runtime where a wedged probe could
/// otherwise block a worker for seconds.
pub async fn detect_with_timeout(external_url_var: &str) -> Result<TestBackend, String> {
    let var = external_url_var.to_string();
    let res = tokio::time::timeout(
        std::time::Duration::from_millis(300),
        tokio::task::spawn_blocking(move || detect(&var)),
    )
    .await
    .map_err(|_| "detect_backend timed out after 300ms".to_string())?
    .map_err(|e| format!("detect_backend task panicked: {e}"))?;
    res.map_err(|e| e.to_string())
}
