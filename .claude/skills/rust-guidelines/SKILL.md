---
name: rust-guidelines
description: Rust language and structural conventions for this project — ownership, borrowing, logging, module layout, struct/method naming, and the no-inline-mod rule. Read before writing or editing any Rust code in this repo so your changes stay consistent with the rest of the codebase.
user-invocable: false
---

# Rust guidelines

## Language style

The codebase favours clarity over cleverness. Code is read far more often than written, so optimise for the reader.

- Prefer idiomatic Rust. No "black magic" — no macro tricks, no lifetime gymnastics when plain structures work.
- **Readability over efficiency.** Simple code with `.clone()` beats convoluted borrow graphs. That said, prefer ownership transfer over cloning when possible.
- **Pass `String` by reference as `&str`.** Accept `&str` to read, `String` to take ownership.
- **No `unsafe` in production code.** Enforced via `[workspace.lints.rust] unsafe_code = "deny"`. The only exception: narrow `#[allow(unsafe_code)]` blocks in tests around `std::env::set_var`, each with a `// Why:` comment.
- **Pinned library versions.** All versions explicit in `[workspace.dependencies]`; crates reference them via `workspace = true`.
- **No `unwrap()` in production code.** Enforced via `clippy::unwrap_used = "deny"`. Use `expect("reason")` for infallible cases (static regex, const parsing). Tests are exempt via `#[allow(clippy::unwrap_used)]`.
- Avoid magic numbers — prefer configs.
- Prefer small code duplication over coupling.
- Avoid "nano functions" that perform a single command or a routine operation. Small methods are allowed if they comply
  with OOP.
- use ahash instead of std::collections::HashMap
- **No future-proofing config or struct fields.** Do not introduce TOML keys, struct fields, or enum variants for
  hypothetical future features. Add the field together with the feature that consumes it.
- Do not use complex multi-level nested constructs like Ok( match ...). Try to split such constructs into multiple
  variables. (except for pattern matching extraction — that is acceptable)
- do not make lines and blocks oversaturated — separate into variables and try to follow the one line per expression
  approach
- Do not use abbreviations or acronyms except for commonly accepted ones. Use commonly accepted abbreviations only when
  other alternatives are unwieldy
- Do not use Box::leak in production code
- **Static construction methods:** Prefer `Type::create(...)` as the named constructor, even when returning `Result`. This makes construction explicit and avoids bare `new()` which can hide fallibility.
- **Strict encapsulation:** Do not expose internal data structures through public API. Minimize the public surface of each crate. Internal structs, helper functions, and implementation details must be `pub(crate)` or private.

## Timeouts and cancellation safety

- Connector adapters own cancel-safety, not the runner. The runner only wraps each call in `tokio::time::timeout` + a shutdown `select!`.
- For drivers that are not cancellation-safe (notably `mongodb` 3.x — dropping a future mid-await can leave driver internals inconsistent), wrap the driver call in `air_elt_commons_mongodb::task::detached`. It spawns the work on the runtime so dropping the outer future does not cancel the driver future.

## Logging and errors

All observability goes through `tracing`. Errors must never be silently discarded.

- Use `tracing::{info, warn, error, debug, trace}` with structured fields. No `println!` / `eprintln!` in libraries. No `#[tracing::instrument]` — emit explicit logs at meaningful boundaries instead.
- Initialise the subscriber once in `app::main`. Other crates only emit events.
- **Never swallow errors.** `let _ = …` / `.ok()` / `.unwrap_or_default()` on `Result` require a preceding `warn!` / `error!` with context.
- **`thiserror` in libraries, `anyhow` only in `app`.** Preserve the `source` chain.

## Module layout

Consistent structure keeps navigation predictable across crates.

- **Directory modules have an explicit `mod.rs`** containing only `mod` / `pub mod` / `pub use` — no other code.
- **No inline modules** (`mod foo { … }`). The single exception is `#[cfg(test)] mod tests`.
- **Nesting depth ≤ 2 levels from crate root; 1 preferred.** Example:
  ```
  src/
  ├── lib.rs
  ├── config/{mod.rs, model.rs}
  ├── model/{mod.rs, pg_type.rs, mapping.rs}
  ├── pg_source.rs
  └── sql_statements.rs
  ```

## File and type naming

- **Main struct → matching file name.** `PgSource` lives in `pg_source.rs`. No `util.rs`, `helpers.rs`, `common.rs`.
- **Names say what they do.** Avoid `Handler::handle`, `Manager::manage`. Prefer `PgStorage::validate_access`, `SchemaIntrospector::describe_table`.
- Exception: middleware and callback traits can use generic verbs (`OnBatch::on_batch`).

## Stateful logic belongs in structs

- State across calls → method on a struct. Free functions for pure helpers only.
- Name the struct after its responsibility: `PgStorage`, not `StorageHelper`.

## No hidden contracts, no half-built structs

A struct that exists in code must be fully usable. "Build it now, fill in the rest later via this other function" is a hidden invariant — the type system should refuse to construct an incomplete value, not rely on caller discipline.

## Tests

The test strategy is inverted pyramid: heavy e2e against real services, focused unit tests for pure logic.

- **`tests/` folder is for e2e tests**, `#[cfg(test)] mod tests` is for unit tests.
- **Do not mock databases.** Use `air_elt_commons_testing::pg::pg_pool()`.
- Test-only utilities live in dedicated crates under `[dev-dependencies]` (e.g. `air-elt-commons-testing`). In-crate test helpers go into `#[cfg(test)] mod tests`.
- Tests using the registry/validator need `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`.
- Favour `AIR_ELT_TEST_*` env var names to avoid races between tests.
- **Mocking with `mockall`.** Use `#[cfg_attr(test, mockall::automock)]` on traits in `core::traits`. For methods with `Option<&T>` parameters, add an explicit lifetime — mockall cannot handle elided lifetimes inside `Option`.
- **Deterministic time in tests.** Use `#[tokio::test(start_paused = true)]` instead of real `sleep`/wall-clock waits. This makes tests instant and non-flaky. Ensure mocked sources drain (return empty batch after data) so Once-mode tests terminate.
- **No N×N matrices for connector pairs.** Each source / sink / storage owns its own e2e suite covering its typical cases (types, NULLs, cursors, schema quirks). Cross-vendor pipelines are exercised by a *small, fixed* sample of combinations — enough to prove the runner glues things together — not by every pair. Adding a new connector means adding its own suite plus one or two sample cross-vendor flows, not 2N new combination tests.
- try to avoid "sleep" in tests
- **Shutdown sqlx pools before test exit.** End every `#[tokio::test]` with `handle.pool.close().await` for any sqlx-backed handle. Sync `Pool` Drop on a tearing-down tokio runtime hangs for seconds. (For `MongoTestHandle` do NOT call `Client::shutdown` from tests — it deadlocks against still-live Arc clones; let the runtime tear it down.)
- **Close pools/clients only where it pays.** sqlx: explicit `Pool::close().await` is a big win — saves seconds on the runtime-shutdown hang. mongo: skipping `Client::shutdown` is the win — calling it stalls the test on Arc-clone deadlock.
- **File-lock test infra at the narrowest window.** Cross-process locks (`crate::filelock::acquire_lock`) around container `start()` are required so nextest doesn't race on `reuse=Always`. Hold the lock only across the create-or-reuse call — release before any `wait_for_*`, TCP handshake, or other slow probe so siblings can proceed in parallel.
- **Doctests are disabled workspace-wide.** Every lib crate's `Cargo.toml` must declare `[lib]` with `doctest = false` so `cargo test --workspace` skips doc-tests automatically. Add the section when creating a new crate.
- **Consolidate test files into one binary per crate.** Use `autotests = false` in `Cargo.toml` and a single `tests/all.rs` that does `mod foo; mod bar;` for each test file. Cuts cargo's per-binary serialization overhead.
- **No hidden tests.** `cargo test` must run ALL tests. Temporarily disabled flag/env-gated tests and silent skips are not
  allowed.

## After every change

- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`.
- Add or update tests unless the change is documentation or metrics-only.