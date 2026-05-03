//! Pipeline orchestration helper for cross-vendor app tests.
//!
//! Inlining the load → assemble → migrate → validate → engine boilerplate
//! four times across `tests/` adds ~20 LOC per file with no value, so we
//! collapse it into a single `run_once` driver here. Deliberately small
//! surface: no test-framework abstraction, no generics, no traits.

#![allow(clippy::unwrap_used, dead_code)]

use std::path::Path;

use air_elt_app::registry::build_registry;
use air_elt_core::config::loader;
use air_elt_core::flow::engine::FlowEngine;
use air_elt_core::flow::runner::RunMode;
use air_elt_core::validation::pipeline::{assemble, validate};
use tokio::sync::watch;

/// Drive the full pipeline once: load the config, run an initial
/// assemble/validate to invoke `Storage::migrate` (so the sink's access
/// probe lands against a migrated cursor table), then re-assemble +
/// validate + run the engine in `Once` mode. Mirrors what `air-elt run
/// --once` does in production.
///
/// The double assemble/validate is intentional — the first pass exists
/// solely to obtain `Storage` handles for `migrate`; only the second
/// pass's `flows` get fed to the engine.
pub async fn run_once(config_path: &Path) {
    let root = loader::load(config_path).expect("load config");
    let registry = build_registry();

    let assembled_pre = assemble(&root, &registry)
        .await
        .expect("pre-migrate assemble");
    let flows_pre = validate(assembled_pre).await.expect("pre-migrate validate");
    for f in &flows_pre {
        f.storage.migrate().await.expect("migrate");
    }
    drop(flows_pre);

    let assembled = assemble(&root, &registry).await.expect("assemble");
    let flows = validate(assembled).await.expect("validate");
    let (_tx, rx) = watch::channel(false);
    FlowEngine::new(flows, RunMode::Once, rx)
        .run()
        .await
        .expect("engine run");
}
