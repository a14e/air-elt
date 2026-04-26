---
name: project-conventions
description: Mandatory shared utilities and patterns for Air Elt — load before changing any Rust code so you use the right crate helpers instead of writing ad-hoc alternatives. Covers logging, SQL identifier escaping, value binding, config loading, secret resolution, type model, testing, factory wiring, and error types. Update this file whenever a new cross-crate utility is introduced.
---

# Project conventions — mandatory utilities

Before editing Rust code, check this list. If a utility exists for your need, use it. **Do not reimplement.** If you add a new cross-crate helper, append it here.

## Commons isolation

`air-elt-commons` (`crates/commons/lib`) is the foundational utility crate and **MUST NOT depend on any other `air-elt-*` crate**, including `air-elt-core`. It hosts only project-internal-dep-free helpers: `tracing_init`, `identifier` (validation + `IdentifierError`), `pool_timeouts`.

Direction of dependency is the inverse: `core` (and connectors) depend on `commons`, never the other way around. If you find yourself wanting to import `air-elt-core` into commons, the type or impl belongs somewhere downstream — in `core` itself, or in `commons-pg` / `commons-mysql` (which legitimately depend on both core and commons).

Example: `impl From<IdentifierError> for RuntimeError` lives in `core::error`, not in `commons::identifier`, even though it bridges the two — the `From` impl is needed for `?`-ergonomics in connector code, and core is allowed to know about commons.

## Config naming

TOML config keys use **kebab-case** for multi-word fields (`batch-limit`, `operation-timeout-secs`, `max-connections`). Structs with multi-word fields carry `#[serde(rename_all = "kebab-case")]`.

## Logging

Initialise the subscriber once via `air_elt_commons::tracing_init::init()` in `app::main`. All style rules (structured fields, no instrument, no println) are in `rust-guidelines`.

## SQL — identifier escaping

All dynamic identifiers in SQL must go through these helpers. Raw `format!` quoting is forbidden.

- Validation primitives in **`air_elt_commons::identifier`** (`IdentifierError`, `is_bare_ident_char`, `validate_segment`) — db-agnostic.
- pg quoting (`"`): **`air_elt_commons_pg::identifier::{quote_ident, quote_qualified, quote_columns, split_qualified}`** (`split_qualified` defaults to `public`).
- mysql quoting (backtick): **`air_elt_commons_mysql::identifier::{quote_ident, quote_qualified, quote_columns, split_qualified}`** (`split_qualified` returns `(Option<db>, table)` — bare names default to `SELECT DATABASE()`).
- `IdentifierError → RuntimeError` via `impl From` (lives in `core::error`) — use `?` directly.

## SQL — type tables

Source-side type resolution: native type → canonical `DataType`. Each table folds the column's declared length into `Text { size }` / `Bytes { size }` (or unbounded for `text`/`blob`-family).

- **`air_elt_commons_pg::pg_type::{PgType, parse, to_internal}`** — accepts `timestamptz` only; naive `timestamp` returns `None`.
- **`air_elt_commons_mysql::mysql_type::{MySqlType, parse, to_internal}`** — `tinyint(1)` → `Bool`, other tinyints → `Int16`. `datetime` is rejected; only `timestamp` is accepted (UTC).

## SQL — NULL binding

Typed NULL binding ensures the wire type matches the column.

- **`air_elt_commons_pg::null_bind::bind_typed_null(query, DataType)`**
- **`air_elt_commons_mysql::null_bind::bind_typed_null(query, DataType)`**
- Sink-side: the NULL match is inlined inside `push_values` (the `Separated` lifetime prevents extraction).

## SQL — pool construction

All postgres / mysql connectors open pools through a shared helper. Both reuse the same `PoolTimeouts` (db-agnostic).

- **`air_elt_commons::pool_timeouts::PoolTimeouts`** — provider-agnostic struct (`defaults()`, `from_options(...)`).
- **`air_elt_commons_pg::pool::connect(url, PoolTimeouts)`** — wires `SET TIME ZONE 'UTC'` + `SET statement_timeout`.
- **`air_elt_commons_mysql::pool::connect(url, PoolTimeouts)`** — wires `SET SESSION time_zone='+00:00'` + `SET SESSION max_execution_time`.
- **Defaults**: connect 5s, acquire 10s, idle 300s, max_lifetime 1800s, statement 30s, max_connections 5.

## SQL — schema introspection

- **`air_elt_commons_pg::schema::fetch_schema(pool, table)`** — pg `information_schema.columns` (reads `character_maximum_length` for sized text/bytes).
- **`air_elt_commons_mysql::schema::fetch_schema(pool, table)`** — mysql `information_schema.COLUMNS` (reads `column_type` for `tinyint(1)` discrimination + `character_maximum_length`).

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

- **`air_elt_core::types::{DataType, Value}`** — the only type enums. Never introduce parallel enums in connectors. `Text`/`Bytes` carry `size: Option<u32>`. `BigInt { width }` and `Decimal { precision, scale }` cover SQL `numeric`/`decimal`: scale-0 columns map to `BigInt` (carrying `num_bigint::BigInt`), scale > 0 to `Decimal` (carrying `bigdecimal::BigDecimal`). Sources read both via sqlx `BigDecimal`; the BigInt arm extracts the integer mantissa with no arithmetic.
- Source owns `native → DataType`, sink owns `DataType → native`. Shared halves (`PgType`, `MySqlType`, `parse`, `to_internal`) in `commons-pg` / `commons-mysql`.
- **`air_elt_core::types::matrix::is_compatible(source_dt, sink_dt)`** — validation-time width check (no narrowing, no unbounded→bounded), plus `Uuid ↔ Text/Bytes`, `Int* ↔ Bool`, `Int*/BigInt → BigInt/Decimal` widening allowances, `Json/Xml → Text*` (unbounded), `Text → Bool` (lexer), `Text → Xml` (well-formed). Reverse paths (`BigInt/Decimal → Int*`, `Float ↔ BigInt/Decimal`) are deliberately rejected by the lossless matrix.
- **`air_elt_core::types::matrix::is_compatible_with_truncate(src, dst)`** — wider matrix used when a mapping has `truncate=true`. Admits text/bytes narrowing, integer/float saturating narrowing, signed↔unsigned, decimal scale drop, BigInt/Decimal → integer, `Json/Xml → Text(n)`, `Timestamp → Date`. Explicitly forbids `Json→Json`, `Xml→Xml`, UUID truncations, `Date→Timestamp`.
- **`air_elt_core::types::convert::{convert, ConvertError, ConversionContext}`** — dispatcher `convert(value, src, dst, &ctx)` for runtime per-cell conversion. Identity / pure-widening pairs return the value unchanged. UUID parsing accepts canonical `8-4-4-4-12`, hex-only, MS-style `{...}`. `ctx.truncate=true` opts into narrowing arms; `ctx.default=Some(v)` substitutes when the source value is `Null`. Connectors must NOT implement these conversions themselves.
- **`air_elt_core::types::convert::truncate_utf8`** — UTF-safe byte-prefix helper. Always cuts at the last complete codepoint ≤ `max_bytes`.
- **`air_elt_core::types::convert::saturate::*`** — saturating numeric primitives (`sat_i64_to_i32`, `sat_bigint_to_width`, `sat_f64_to_i64`, `bigdecimal_to_bigint_truncating`, …). Never panic; clamp to the target's representable range.
- **`air_elt_core::types::default_value::parse(literal, sink_dt)`** — parses a TOML default literal against the sink `DataType`. Bytes columns require a typed prefix (`hex:` / `base64:` / `utf8:` / `bin:`). Other types use the plain literal.
- **`FlowState::conversions: Vec<ConversionPlan>`** — populated by `validation::pipeline::validate`. Each `ConversionPlan { source, sink, ctx }` carries the truncate flag and the parsed default; the runner skips identity plans and dispatches the rest through `convert` with the per-column ctx.

## Testing

- **`air_elt_commons_testing::pg::pg_pool()`** — `PgTestHandle` with sandboxed schema. Honours `AIR_ELT_TEST_PG_URL` or auto-detects podman/docker.
- **`air_elt_commons_testing::mysql::mysql_pool()`** — `MySqlTestHandle` with sandboxed database. Honours `AIR_ELT_TEST_MYSQL_URL` or auto-detects podman/docker.
- **`air_elt_commons_testing::mariadb::mariadb_pool()`** — `MariaDbTestHandle` for the MariaDB test target of the mysql connector (validates legacy `VALUES()` UPSERT and native UUID divergences). Honours `AIR_ELT_TEST_MARIADB_URL` or auto-detects podman/docker. Uses an extra connect-retry loop to absorb the MariaDB image's bootstrap-then-restart sequence.
- Keep the handle alive for the whole test — dropping it tears down the schema/database.
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