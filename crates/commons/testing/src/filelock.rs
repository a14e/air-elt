//! Cross-process file locks used by the test infra.
//!
//! Under `cargo nextest run` every test runs in its own process; multiple
//! processes simultaneously call into the test handle factories
//! (`pg_pool` etc.). The factories ultimately reach `image.start()` on a
//! shared, named container. testcontainers' `reuse=Always` is meant to
//! deduplicate these calls, but the daemon-side check is not atomic with
//! the create call — the loser of the race gets
//! `container name "..." is already in use`. Holding an exclusive file
//! lock across the create-or-reuse closes that gap.

use std::fs::File;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

/// Acquire an exclusive cross-process lock keyed by `kind` (e.g. `"pg"`,
/// `"mongo"`, `"ryuk"`). Released when the returned `File` is dropped.
pub fn acquire_lock(kind: &str) -> File {
    let dir = std::env::temp_dir().join("air-elt-test-session");
    let _ = std::fs::create_dir_all(&dir);
    let lock_path = dir.join(format!("{kind}-start-{}.lock", target_hash()));
    let f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap_or_else(|e| panic!("open {kind} start-lock {}: {e}", lock_path.display()));
    f.lock()
        .unwrap_or_else(|e| panic!("acquire {kind} start-lock: {e}"));
    f
}

/// Stable per-workspace identifier so parallel checkouts (different
/// worktrees) don't collide on the same lock files. Walks up from the
/// current test exe to find the `target/` parent and hashes that path.
fn target_hash() -> String {
    let workspace = workspace_root_from_current_exe()
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let mut h = Sha256::new();
    h.update(workspace.to_string_lossy().as_bytes());
    let digest = h.finalize();
    hex_short(&digest)
}

fn workspace_root_from_current_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let mut cur = exe.parent()?;
    while let Some(parent) = cur.parent() {
        if cur.file_name().is_some_and(|n| n == "target") {
            return parent.to_path_buf().into();
        }
        cur = parent;
    }
    None
}

fn hex_short(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(16);
    for b in bytes.iter().take(8) {
        s.push_str(&format!("{b:02x}"));
    }
    s
}
