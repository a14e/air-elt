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
- Avoid "nano functions" that perform a single command or a routine operation. Small methods are allowed if they comply with OOP.
- use ahash instead of std::collections::HashMap
- **No future-proofing config or struct fields.** Do not introduce TOML keys, struct fields, or enum variants for hypothetical future features. Add the field together with the feature that consumes it. The previous "object form mapping" with reserved `transform` / `timezone` / `data-type` was a concrete example of this anti-pattern — reserved fields rot and create misleading parser surface. If a feature is on the near horizon, file an `agent_tasks/` ticket; don't pre-wire the surface.

## Timeouts and cancellation safety

- When wrapping a future in `tokio::time::timeout` or `tokio::select!`, verify the underlying driver/protocol supports cancellation without leaving inconsistent state. sqlx postgres queries are cancellation-safe (drop sends a cancel message to the server). If a driver is not cancellation-safe, document the risk and consider `spawn` + `abort` instead of `select!`.

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

## After every change

- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`.
- Add or update tests unless the change is documentation or metrics-only.