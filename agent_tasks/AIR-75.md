# Structural self-linting

This task adds a self-contained structural linter as a workspace CLI crate.

## What we do
* Add a self-lint CLI crate (`crates/self-lint`) with zero workspace-internal dependencies (only minimal external deps)
* Integrate it into the Justfile (separate recipe that `lint` depends on)
* Make sure it runs in CI

## What we validate
1. **Language.** All files use ASCII only; emit an error that all files must be in English (written by human or AI).
2. **Workspace dependency graph:**
   a. All commons crates depend only on other commons (or core for DB-specific commons). No sources/sinks/storages in commons.
   b. Nobody depends on `app`.
   c. Sinks don't depend on each other or on sources/storages.
   d. Sources don't depend on each other or on sinks/storages.
   e. Storages don't depend on each other or on sinks/sources.
   f. `types` depends on nobody except possibly commons.
   g. `core` does not depend on sinks, sources, or storages.
   h. `monitoring` depends on nobody except possibly commons.
   i. The self-linter depends on no workspace crate at all (including commons).
3. **Factory registration.** Every source/sink/storage crate has at least one factory registered in `app/src/registry.rs`.
4. **Trait implementations.** Each source crate implements Source + SourceFactory + SourceCtx; each sink crate implements Sink + SinkFactory + SinkCtx; each storage crate implements Storage + StorageFactory.
5. **App test coverage.** Every registered connector type (postgres, mysql, mongodb, mongo-cdc, clickhouse, questdb, cockroachdb) is mentioned in `app/tests/`.
6. **Version bump.** Workspace version must be higher than in `origin/main`. The bump must be valid semver. Patch bump after each task; minor bump only for backward-incompatible changes; major bump only by user decision.
7. **Testcontainer naming.** In `commons/testing`, every container creation must include `with_container_name` (needed for ryuk cleanup).
8. **CI env var coverage.** Every `std::env::var("AIR_ELT_TEST_*")` in `commons/testing` must have a corresponding entry in the CI workflow.
9. **Module file purity.** All `mod.rs` and `lib.rs` files must contain only imports, aliases, and module declarations — no code. They serve a pure package function.
10. **Test aggregator.** Every crate with a `tests/` directory must have `tests/all.rs`, plus `autotests = false` and `[[test]] name = "all" path = "tests/all.rs"` in its Cargo.toml.
11. **Doctest disabled.** Every lib crate must have `[lib] doctest = false`.

## Other
1. Translate this task file to English before starting (done).
2. Build the linter in release mode before running it.
3. In CI, the binary is cached via `Swatinem/rust-cache` and build artifacts are cleaned after build.
4. Implement as a CLI, not as a test.
5. Minimal unit tests — the application itself running against the real repo is the test.
