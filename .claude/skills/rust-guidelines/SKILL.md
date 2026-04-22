---
name: rust-guidelines
description: Rust language and structural conventions for this project — ownership, borrowing, logging, module layout, struct/method naming, and the no-inline-mod rule. Read before writing or editing any Rust code in this repo so your changes stay consistent with the rest of the codebase.
---

# Rust guidelines

## Language style

- **Use best practices.** Prefer idiomatic Rust; don't try to be clever with lifetimes or macros when ordinary structures work.
- **Readability over efficiency.** Simple, obviously-correct code with a `.clone()` is preferred over convoluted borrow graphs. We have budget to optimise later; debugging subtle reference juggling now is not worth it.
- **That said, prefer ownership transfer over cloning.** If you can give the value away, do. `clone` is a tool for when ownership is genuinely shared, not a default.
- **Pass `String` by reference as `&str`.** Functions that only read accept `&str`; functions that take ownership take `String`.
- **No `unsafe` in production code.** Enforced workspace-wide via `[workspace.lints.rust] unsafe_code = "deny"`. The only permitted `unsafe` is narrow `#[allow(unsafe_code)]` blocks in test code around edition-2024 `std::env::set_var` calls — each one must carry a `// Why:` comment explaining the race-safety argument. Nothing else.
- **Pinned library versions.** All crate versions are explicit in `[workspace.dependencies]`. Individual crates refer to the workspace entries via `workspace = true`.
- DO NOT use "black magic". Code should be clean and obvious. Avoid unnecessary complexity and cleverness; keep the codebase readable and maintainable.
- Avoid magic numbers. Prefer configs instead.

## Logging and errors

- **`tracing` only.** `println!` / `eprintln!` are forbidden in library code. Use the `tracing::{info, warn, error, debug, trace}` macros with structured fields (`field = %value` / `field = ?value`).
- **No `#[tracing::instrument]` in this project.** The ELT engine is a flat read-batch / write-batch loop — automatic span hierarchies don't carry useful information for us. Emit explicit `info!`/`warn!`/`error!` with named fields (`flow`, `table`, `rows`, `cursor_field`) at meaningful boundaries instead.
- **Initialise once, in `app::main`.** Every other crate just emits events.
- **Never swallow errors silently.** `let _ = …`, `result.ok()`, `unwrap_or_default()` on a `Result` are not allowed unless the branch has already emitted `tracing::warn!` / `error!` with context. If an error is non-fatal, log it; if it's fatal, bubble it up with `?`.
- **`thiserror` in libraries, `anyhow` only in `app`.** Library errors must preserve the `source` chain so that top-level logs print the full cause.

## Module layout

- **Directories are modules with an explicit `mod.rs`.** `mod.rs` contains **only** `mod x;` / `pub mod x;` / `pub use x::…;` declarations — **no other code** (no types, no functions, no constants).
- **No inline modules inside `.rs` files.** `mod foo { … }` is forbidden. Split into a separate file instead. The single exception is `#[cfg(test)] mod tests { … }` for unit tests.
- **Module nesting depth ≤ 2 levels from crate root; 1 is preferred.** Example (sources/postgres):
  ```
  src/
  ├── lib.rs
  ├── config/{mod.rs, model.rs}
  ├── model/{mod.rs, pg_type.rs, mapping.rs}
  ├── pg_source.rs
  └── sql_statements.rs
  ```

## File and type naming

- **Main struct file uses the struct's name.** `PgSource` lives in `pg_source.rs`, not `impl_.rs`. File names describe what's inside; no generic names like `util.rs`, `helpers.rs`, `common.rs`.
- **Method and type names must say what they do — not what category they belong to.** Avoid `Handler::handle`, `Processor::process`, `Manager::manage`, `Runner::run` as the *primary* name. Prefer specifics: `PgStorage::validate_access`, `FlowRunner::drain_once`, `SchemaIntrospector::describe_table`.
- **Exceptions to the naming rule:** middleware, metric emitters, and callback-style traits can use generic verbs (`Middleware::handle`, `OnBatch::on_batch`) because the abstract shape *is* their purpose. Everywhere else, be specific.

## Stateful logic belongs in structs

- **If a function carries state across calls, make it a method on a struct.** Free functions are fine for pure helpers (SQL builders, format conversions, parsers), but anything holding a connection, a cursor, a cache, or a configuration should be a struct with named methods.
- **Name the struct after its responsibility.** `PgStorage`, not `StorageHelper`.

## Tests

- **Inverted pyramid.** Prefer e2e against real services over unit tests with mocks. Keep unit tests for pure logic (matrices, parsers, SQL builders).
- **Tests in `tests/` folder per crate, or `#[cfg(test)] mod tests` inline.** Nothing else. Don't create a parallel `test_utils` module in library code.
- **Do not mock databases.** Use `air_elt_commons_testing::pg::pg_pool()`.
- **Test-only code lives in a dedicated crate.** `air-elt-commons-testing` is a sibling crate in `crates/commons/testing/` — consumers add it only under `[dev-dependencies]`. Listing it as a regular dependency would ship `testcontainers`, `sqlx`, and the probe-socket helpers into release builds. In-crate test helpers use `#[cfg(test)] mod tests`; do not expose them as public items.
- **`unsafe { set_var(...) }` in tests** is the one permitted use of `unsafe`: wrap it in `#[allow(unsafe_code)]` with a `// Why:` comment. Favour process-environment-unique variable names (`AIR_ELT_TEST_*`) so no other test in the crate can race against them.
- **Tests that talk to postgres via the registry / validator** must use `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]` — single-threaded runtimes deadlock the async factory path.

## After every change

- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`.
- Add or update tests unless the change is documentation or metrics-only.
