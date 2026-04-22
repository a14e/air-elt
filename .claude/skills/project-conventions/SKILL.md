---
name: project-conventions
description: Mandatory shared utilities and patterns for Air Elt — load before changing any Rust code so you use the right crate helpers instead of writing ad-hoc alternatives. Covers logging, SQL identifier escaping, value binding, config loading, secret resolution, type model, testing, factory wiring, and error types. Update this file whenever a new cross-crate utility is introduced.
---

# Project conventions — mandatory utilities

Before editing Rust code in this repo, check what's listed here. If a utility exists for your need, use it. **Do not reimplement** any of the items below. If you add a new cross-crate helper, append it here.

## Logging — `tracing`

- Use `tracing::{info, warn, error, debug, trace}` macros with structured fields (`table = %spec.table`, `rows = report.rows_written`). No `#[tracing::instrument]` anywhere — our flat batch loop does not benefit from automatic span hierarchies.
- Initialise the subscriber exactly once in `app::main` via `air_elt_commons::tracing_init::init()`. No crate should set up its own subscriber.
- Forbidden: `println!`, `eprintln!` (outside of clap help/error printing), the `log` crate, silent `let _ = result`, `result.ok()` without a preceding warn/error.

## SQL — identifier escaping

- **`air_elt_commons::sql::pg::identifier::quote_ident(name)`** — single-identifier quoting, doubles internal `"`.
- **`air_elt_commons::sql::pg::identifier::quote_qualified(name)`** — dotted form like `schema.table`. The dot is emitted *outside* the double quotes: `schema.table` → `"schema"."table"`. Segments containing characters outside `[A-Za-z0-9_$]` are rejected.
- **`air_elt_commons::sql::pg::identifier::quote_columns(&[String])`** — comma-joined quoted list for `SELECT`/`INSERT` column lists.
- **`air_elt_commons::sql::pg::identifier::split_qualified(name)`** — splits `schema.table` → `(schema, table)`; used where `information_schema` queries need the two parts bound separately.
- `IdentifierError` converts to `RuntimeError` via `?` thanks to `impl From<IdentifierError>` in commons — do not write ad-hoc wrapping.

## SQL — PG type mapping

- **`air_elt_commons::sql::pg::pg_type::{PgType, parse, to_internal}`** — the canonical list of PG native types and the one-way map to `DataType`. The reverse (`from_internal`) is sink-specific and lives in `sinks/postgres::model::pg_type`.
- **`timestamp` without time zone is deliberately unsupported** — `PgType::parse("timestamp")` returns `None`. Operators must migrate to `timestamptz`. Naive timestamps re-interpret under session TimeZone and are a silent data-shift hazard.

## SQL — NULL binding

- **`air_elt_commons::sql::pg::null_bind::bind_typed_null(query, DataType)`** and **`push_typed_null(tuple, DataType)`** pick the right `Option::<T>::None` per canonical type so the bind OID matches the column OID. Any raw `query.bind::<Option<i64>>(None)` against a non-`bigint` column is a bug.

## SQL — pool construction

- **`air_elt_commons::sql::pg::pool::connect(url, PoolTimeouts)`** — all three postgres connectors open pools through this. It wires `connect_timeout` / `acquire_timeout` / `idle_timeout` / `max_lifetime` plus `SET TIME ZONE 'UTC'` and `SET statement_timeout = …` in `after_connect`.
- **Defaults**: `PoolTimeouts::defaults()` — connect 5s, acquire 10s, idle 300s, max_lifetime 1800s, statement 30s. Per-connector overrides come from `*Config.{connect,acquire,idle,max_lifetime,statement}_timeout_secs`.

## SQL — value binding

- Always bind values via sqlx `$N` placeholders and `query.bind(value)` / `QueryBuilder::push_bind`. **Never** interpolate values into the SQL string.
- SQL statements are **composed once** at flow setup. Each connector owns its `sql_statements.rs` (e.g. `crates/storages/postgres/src/sql_statements.rs`). No ad-hoc SQL in business-logic files — if you need a new statement, add a helper there.

## Secrets and env vars

- `${VAR}` / `${VAR:default}` placeholders are resolved **at config-load time** by `air_elt_core::config::env_expand::expand`. Lookup order: process env → `[secrets]` map → default clause → error. The resolver runs on the raw TOML text before the main parse; connectors see fully-resolved values.
- `[secrets]` is a `BTreeMap<String, String>` of literals. No recursion, no vault. Vault integration is tracked separately.
- Runtime usage of `std::env::var(...)` in connectors is forbidden — if you need a runtime string, thread it through the config.

## Config loading

- Use `air_elt_core::config::loader::load(path)`. It enforces: 16 MiB file-size cap, absolute-path reject in `include`, symlink-loop dedupe via canonical paths, `${VAR}` expansion, structural validation (`batch_limit ≥ 1`, `batch_limit × mapping_cols ≤ 60_000`, cursor fields subset of mapping, no `UnsupportedInMvp` markers).
- Config types live in `air_elt_core::config::model`.

## Types and the N+N matrix

- Canonical types: `air_elt_core::types::{DataType, Value}`. Never introduce a parallel enum in a connector.
- Each connector owns `native → DataType` on the source side and `DataType → native` on the sink side. The shared half (`PgType`, `parse`, `to_internal`) lives in commons; only sink-specific `from_internal` sits under `sinks/postgres/model/pg_type.rs`.
- Compatibility predicate: `air_elt_core::types::matrix::is_compatible(source_dt, sink_dt)` — identity + safe widening + null-assignability. Used only at validation time.
- No canonical↔canonical value conversion: if types don't match, validation fails.

## Testing

- Use `air_elt_commons_testing::pg::pg_pool()` to get a `PgTestHandle` with a sandboxed `PgPool`. Backend auto-detection order: `AIR_ELT_TEST_PG_URL` → `DOCKER_HOST` → `/var/run/docker.sock` → rootless podman socket → macOS podman-machine socket under `$TMPDIR/podman/*-api.sock`. The detect pass runs inside `spawn_blocking` with a 300 ms timeout so a stale socket cannot block a tokio worker.
- `pg_pool()` self-heals the shared backend at startup: any `test_<unix_ts>_*` schema older than 24 hours is dropped before the current test's sandbox is created.
- `PgTestHandle` exposes `.pool`, `.url`, `.schema`, and `.url_with_search_path()`. Keep the handle alive for the whole test — dropping it early tears down the sandbox schema.
- Mocks of databases are forbidden.
- **Enable via `[dev-dependencies]` only**: `air-elt-commons-testing = { workspace = true }`. The crate pulls in `testcontainers`, `sqlx`, etc. — listing it under `[dependencies]` ships them into release builds. The production `air-elt-commons` crate contains only prod utilities and carries `forbid`-strict lints.
- Tests that hit postgres via the registry / validator use `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`.

## Traits and the runtime contract

- `air_elt_core::traits::{Source, Sink, Storage}` — `#[async_trait]`, object-safe (`Box<dyn Source>`). Do **not** change their signatures without updating every connector and the runner.
- `Batch { rows, next_cursor }`, `Row { values: Vec<Value> }`, `ReadSpec`, `WriteSpec`, `WriteReport`, `CursorState` all live in `core::{traits, flow::state}`. Reuse these types — connectors must not define their own batch/row structs.

## Connector registration (factories)

- Factories are `#[async_trait]` traits: `core::registry::{SourceFactory, SinkFactory, StorageFactory}`, each with a single `async fn build(&self, cfg: &ComponentConfig)`. Registration stores `Arc<dyn SourceFactory>` (etc.), not closures.
- Wire every connector in `app::registry::build_registry()` via zero-sized unit structs (`struct PgSourceFactory;` with `impl SourceFactory`). No sync wrappers, no `block_in_runtime`.
- Do not construct `PgSource`/`PgSink`/`PgStorage` directly from flow code — go through the registry.

## Runner and operation timeouts

- `core::flow::runner::run_all_flows` is the single fan-out entry point (app imports it directly). It wraps every `read_batch`/`write_batch`/`save_cursor`/`load_cursor` call in `tokio::time::timeout` + `tokio::select!` with the shutdown watcher.
- `operation_timeout_secs` on `FlowConfig` overrides the 30-second default. Pool and statement timeouts are set on `PgXConfig`.

## Cursor semantics

- Cursor comparison uses engineer-intuitive "NULL > everything" algebra, matching Postgres default `ORDER BY` (ASC NULLS LAST / DESC NULLS FIRST).
- The fast path uses standard `(c1, c2) > ($1, $2)` SQL when the cursor state is entirely non-null. If any cursor field is NULL, the SQL rewrites to an explicit null-aware predicate so pipelines don't stall on three-valued logic.
- `CursorOrder::Desc` emits `ORDER BY c1 DESC, c2 DESC` (direction per column). Do not rely on SQL's "direction applies to the last column" default.

## Errors

- `thiserror` in library crates. `anyhow` is allowed only in `app`.
- Wrap third-party errors with `RuntimeError::backend(err)` so the `source` chain is preserved.
- Every error variant must have a useful `Display` that includes the relevant context (flow, table, column, cursor field).
- Dedicated variants: `ValidationError::NullabilityMismatch`, `TypeError::NullSinkColumn`, `ConfigError::{UnresolvedReference, ConfigTooLarge, AbsoluteIncludeNotAllowed}`. Do not reuse `UnsupportedCast` for nullability or unrelated problems.

## After changes

- `cargo fmt --all`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- Add a line to this file if you introduced a new utility others must use.
