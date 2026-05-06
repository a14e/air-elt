//! Pipeline orchestration helper for cross-vendor app tests.
//!
//! Thin wrapper over `air_elt_app::App::run_once` so the tests share the
//! exact same wiring path as `air-elt run --once` does in production.

#![allow(clippy::unwrap_used, dead_code)]

use std::path::Path;

use air_elt_app::App;

/// Drive the full pipeline once: load the config, run migrations, and
/// execute the engine in `Once` mode. Equivalent to `air-elt run --once`.
pub async fn run_once(config_path: &Path) {
    App::from_path(config_path)
        .expect("from_path")
        .run_once()
        .await
        .expect("run_once");
}
