---
name: project-conventions
description: Mandatory shared utilities and patterns for Air Elt — load before changing any Rust code so you use the right crate helpers instead of writing ad-hoc alternatives. Covers logging, SQL identifier escaping, value binding, config loading, secret resolution, type model, testing, factory wiring, and error types. Update this file whenever a new cross-crate utility is introduced.
---

# Project conventions — mandatory utilities

Before editing Rust code, check this list. If a utility exists for your need, use it. **Do not reimplement.** If you add a new cross-crate helper, append it here.

## Config naming

TOML config keys use **kebab-case** for multi-word fields (`batch-limit`, `operation-timeout-secs`, `max-connections`). Structs with multi-word fields carry `#[serde(rename_all = "kebab-case")]`.

## Logging

Initialise the subscriber once via `air_elt_commons::tracing_init::init()` in `app::main`. All style rules (structured fields, no instrument, no println) are in `rust-guidelines`.

## SQL — identifier escaping

All dynamic identifiers in SQL must go through these helpers. Raw `format!("\"{}\"", name)` is forbidden.

- **`quote_ident(name)`** — single identifier, doubles internal `"`.
- **`quote_qualified(name)`** — `schema.table` → `"schema"."table"`. Rejects chars outside `[A-Za-z0-9_$]`.
- **`quote_columns(&[String])`** — comma-joined quoted list.
- **`split_qualified(name)`** — `schema.table` → `(schema, table)` for `information_schema` binds.
- `IdentifierError` converts to `RuntimeError` via `impl From` — use `?` directly.

All live in `air_elt_commons::sql::pg::identifier`.

## SQL — PG type mapping

Source-side type resolution: PG native type → canonical `DataType`.

- **`air_elt_commons::sql::pg::pg_type::{PgType, parse, to_internal}`** — shared between source and sink.
- **`timestamp` without time zone is unsupported** — `parse("timestamp")` returns `None`. Only `timestamptz` is accepted.

## SQL — NULL binding

Typed NULL binding ensures the wire OID matches the column type.

- **`air_elt_commons::sql::pg::null_bind::bind_typed_null(query, DataType)`** — for cursor comparisons in source.
- Sink-side: the NULL match is inlined inside `push_values` (the `Separated` lifetime prevents extraction).

## SQL — pool construction

All postgres connectors open pools through a single shared helper.

- **`air_elt_commons::sql::pg::pool::connect(url, PoolTimeouts)`** — wires timeouts, `SET TIME ZONE 'UTC'`, and `SET statement_timeout` in `after_connect`.
- **Defaults**: connect 5s, acquire 10s, idle 300s, max_lifetime 1800s, statement 30s, max_connections 5. Per-connector overrides via `*Config` fields.

## SQL — schema introspection

- **`air_elt_commons::sql::pg::schema::fetch_schema(pool, table)`** — shared for source and sink. Do not duplicate.

## SQL — value binding

- Bind values via sqlx `$N` + `query.bind()` / `QueryBuilder::push_bind`. Never interpolate values into SQL.
- Each connector owns its `sql_statements.rs`. No ad-hoc SQL in business-logic files.

## Interval parsing

- **`air_elt_core::config::interval::{parse, deserialize, serialize}`** — parses `1s`, `1h30m`, `PT1H5S`, `P1W`, `1 second`, etc. into `Duration`. ISO 8601 via `iso8601-duration` crate (routed by `P`/`p` prefix); human-time is built-in with enforced unit order (w>d>h>m>s>ms). Used in `CursorConfig::interval` via serde hooks.

## Secrets and env vars

Config-time resolution of `${VAR}` / `${VAR:default}` placeholders.

- **`air_elt_core::config::env_expand::expand`** — runs on raw TOML before parsing. Lookup: env → `[secrets]` map → default → error.
- `[secrets]` is a literal `BTreeMap<String, String>`. No recursion, no vault (tracked separately).
- `std::env::var(...)` in connectors is forbidden — thread runtime strings through the config.

## Config loading

- **`air_elt_core::config::loader::load(path)`** — enforces: 16 MiB file-size cap, absolute-path reject in `include`, symlink-loop dedupe, `${VAR}` expansion, structural validation (`batch_limit ≥ 1`, `batch_limit × mapping_cols ≤ 60_000`, cursor fields ⊆ mapping).
- Config types: `air_elt_core::config::model`.

## Types and the N+N matrix

Canonical types avoid direct source↔sink coupling — each connector maps to/from the shared pivot.

- **`air_elt_core::types::{DataType, Value}`** — the only type enums. Never introduce parallel enums in connectors.
- Source owns `native → DataType`, sink owns `DataType → native`. Shared half (`PgType`, `parse`, `to_internal`) in commons.
- **`air_elt_core::types::matrix::is_compatible(source_dt, sink_dt)`** — validation-time only. No runtime conversion.

## Testing

- **`air_elt_commons_testing::pg::pg_pool()`** — returns a `PgTestHandle` with a sandboxed schema. Auto-detects backend (external URL or local container via podman/docker).
- Keep the handle alive for the whole test — dropping it tears down the schema.
- **`[dev-dependencies]` only** — listing it under `[dependencies]` ships testcontainers into release builds.
- Database mocks are forbidden.
- **`mockall` (dev-dependency)** — used for unit-testing runner logic. traits carry `#[cfg_attr(test, mockall::automock)]`.

## Traits and runtime contract

- **`air_elt_core::traits::{Source, Sink, Storage}`** — `#[async_trait]`, object-safe. Do not change signatures without updating all connectors and the runner.
- Shared types: `Batch { rows, next_cursor }`, `Row { values }`, `ReadSpec`, `WriteSpec`, `WriteReport`, `CursorState`. Connectors must not define their own.

## Connector registration

Factories are `#[async_trait]` traits in `core::registry`, each with `async fn build(&self, cfg: &ComponentConfig)`. Registration stores `Arc<dyn *Factory>`.

Wire connectors in `app::registry::build_registry()` via zero-sized structs (`struct PgSourceFactory;`). Do not construct connectors directly from flow code.

## Engine and timeouts

- **`core::flow::engine::FlowEngine`** — fan-out entry point. `FlowEngine::new(flows, mode, shutdown).run()` spawns one `FlowRunner` per flow. Each runner wraps every DB call in `tokio::time::timeout` + `tokio::select!` with shutdown watcher.
- **`core::flow::runner::FlowRunner`** — per-flow tick loop with exponential backoff (pub(crate), used by FlowEngine).
- `query_timeout` on `FlowConfig` overrides the 30s default.

## Errors

Dedicated error variants — use the right one instead of generic `RuntimeError::Other`.

- Wrap third-party errors with `RuntimeError::backend(err)` to preserve the `source` chain.
- `ValidationError::NullabilityMismatch`, `TypeError::NullSinkColumn`, `ConfigError::{UnresolvedReference, ConfigTooLarge, AbsoluteIncludeNotAllowed}`.

## After changes

- Add a line to this file if you introduced a new utility others must use.