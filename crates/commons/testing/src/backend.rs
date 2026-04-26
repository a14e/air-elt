//! Container-runtime detection shared between the pg and mysql test helpers.
//!
//! The whole point: testcontainers reads `DOCKER_HOST` on first call. If we
//! don't probe and export it before that call, it falls back to a stale
//! `/var/run/docker.sock` and hangs for tens of seconds. This module probes
//! the standard locations (docker, rootless podman, macOS podman) up front,
//! returns whatever socket actually accepts a connection, and lets the
//! caller export it.
//!
//! `prepare_container_env(socket)` exports both `DOCKER_HOST` (so
//! testcontainers picks it up) and `TESTCONTAINERS_RYUK_DISABLED=false`
//! (ryuk works fine with our podman setup and prevents orphaned containers
//! after interrupted tests).

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
     - DOCKER_HOST=unix:///path/to/docker-or-podman.sock  (testcontainers)\n\n\
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
    if let Some(path) = which_container_socket() {
        return Ok(TestBackend::Container { socket: path });
    }
    Err(NoBackend)
}

/// Export the container env vars before testcontainers runs.
pub fn prepare_container_env(socket: &str) {
    #[allow(unsafe_code)]
    unsafe {
        // Why: testcontainers reads DOCKER_HOST on first call; we export the
        // auto-discovered socket before it runs so it doesn't fall back to a
        // stale /var/run/docker.sock and hang.
        if std::env::var_os("DOCKER_HOST").is_none() {
            std::env::set_var("DOCKER_HOST", socket);
        }
        if std::env::var_os("TESTCONTAINERS_RYUK_DISABLED").is_none() {
            std::env::set_var("TESTCONTAINERS_RYUK_DISABLED", "false");
        }
    }
}

fn which_container_socket() -> Option<String> {
    if let Ok(host) = std::env::var("DOCKER_HOST")
        && host_is_alive(&host)
    {
        return Some(host);
    }
    if socket_reachable("/var/run/docker.sock") {
        return Some("unix:///var/run/docker.sock".to_string());
    }
    if let Some(uid) = best_effort_uid() {
        let linux_podman = format!("/run/user/{uid}/podman/podman.sock");
        if socket_reachable(&linux_podman) {
            return Some(format!("unix://{linux_podman}"));
        }
    }
    if let Some(path) = scan_macos_podman_sockets() {
        return Some(format!("unix://{path}"));
    }
    None
}

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
        if socket_reachable(path_str) {
            best = Some(path_str.to_string());
            break;
        }
    }
    best
}

fn best_effort_uid() -> Option<u32> {
    std::env::var("UID").ok().and_then(|s| s.parse().ok())
}

fn host_is_alive(host: &str) -> bool {
    if let Some(path) = host.strip_prefix("unix://") {
        socket_reachable(path)
    } else {
        true
    }
}

fn socket_reachable(path: &str) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// Synchronous wrapper that times out the detection probe — handy for
/// callers running inside a tokio runtime where a wedged `UnixStream::connect`
/// could otherwise block a worker for seconds.
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
