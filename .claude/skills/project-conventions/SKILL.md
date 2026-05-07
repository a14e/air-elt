---
name: project-conventions
description: Mandatory shared utilities and patterns for Air Elt — load before changing any Rust code so you use the right crate helpers instead of writing ad-hoc alternatives. Covers logging, SQL identifier escaping, value binding, config loading, secret resolution, type model, testing, factory wiring, and error types. Update this file whenever a new cross-crate utility is introduced.
---

# Project conventions — mandatory utilities

Before editing Rust code, check this list. If a utility exists for your need, use it — **do not reimplement**. Method signatures are not duplicated here; this is a "where to look" map. Add a line when you introduce a new cross-crate helper.

## Commons isolation

`air-elt-commons` (`crates/commons/lib`) is the foundational utility crate and **MUST NOT depend on any other `air-elt-*` crate**, including `air-elt-core`. It hosts only project-internal-dep-free helpers (`tracing_init`, `identifier`, `pool_timeouts`).

Direction of dependency is the inverse: `core` (and connectors) depend on `commons`, never the other way around. If a type wants to bridge `commons` and `core` (e.g. `impl From<IdentifierError> for RuntimeError`), the impl belongs in `core` — `core` is allowed to know about `commons`. The `commons-pg` / `commons-mysql` / `commons-mongodb` crates legitimately depend on both core and commons; `commons-lib` does not.

## Config naming

TOML keys use **kebab-case** for multi-word fields (`batch-limit`, `max-connections`). Structs carry `#[serde(rename_all = "kebab-case")]`. **No future-proofing fields** — every config field must be consumed by the implementation that ships with it.

## Logging

Initialise the subscriber once via `air_elt_commons::tracing_init` in `app::main`. Style rules (structured fields, no instrument, no println) live in `rust-guidelines`.

## SQL helpers

All dynamic SQL identifiers must go through these helpers. Raw `format!` quoting is forbidden.

- **`air_elt_commons::identifier`** — db-agnostic validation primitives + `IdentifierError`.
- **`air_elt_commons_pg::identifier`** — pg quoting (`"`).
- **`air_elt_commons_mysql::identifier`** — mysql quoting (backtick).
- **`IdentifierError → RuntimeError`** via `impl From` in `core::error` — use `?` directly.

Source-side type resolution lives in `commons-pg::pg_type` / `commons-mysql::mysql_type` (native ↔ canonical `DataType`). Notable quirks: pg accepts `timestamptz` only (naive `timestamp` rejected); mysql `tinyint(1)` → `Bool`, other tinyints → `Int16`, `datetime` rejected (only `timestamp` accepted, UTC).

NULL binding goes through `commons-pg::null_bind` / `commons-mysql::null_bind` (extracted helper) on the source side. On the sink side use **`commons-pg::sink_bind::bind_value_separated`** / **`commons-mysql::sink_bind::bind_value_separated`** for binding a `Value` inside a sqlx `Separated` chain. They are shared between the insert (`push_values`) and delete (`push_tuples` for the `(c1,c2) IN ((...))` predicate) paths — do not reimplement per-Value-variant binding inline.

Pool construction goes through `commons-pg::pool` / `commons-mysql::pool`, both consuming `air_elt_commons::pool_timeouts::PoolTimeouts`. They wire UTC time-zone + statement-timeout pragmas. Defaults: connect 5s, acquire 10s, idle 300s, max_lifetime 1800s, statement 30s, max_connections 5.

Schema introspection is `commons-pg::schema::fetch_schema` / `commons-mysql::schema::fetch_schema`. Both read `character_maximum_length`; mysql additionally reads `column_type` for `tinyint(1)` discrimination.

Each connector owns its `sql_statements.rs`. Bind values via sqlx `$N` + `query.bind()` / `QueryBuilder::push_bind` — never interpolate values into SQL.

**Postgres dialect flag (`air_elt_commons_pg::Dialect`)**. The Postgres connector crates serve both `type = "postgres"` and `type = "cockroachdb"`. `PgSourceConfig`/`PgSinkConfig`/`PgStorageConfig` carry a `dialect: Dialect` set by the factory (`PgXxxFactory::postgres()` vs `::cockroach()`); the field is `#[serde(skip)]` so users never touch it. The dialect flag drives only:

- `Dialect::excludes_type(&DataType)` — reject `Xml` columns at `validate_access` for Cockroach (no XML type there).
- `air_elt_commons_pg::retry::with_serialization_retry(dialect, op)` — **mandatory** wrapper around any write-path statement when adding new code paths. On Postgres it's a single-shot pass-through (zero behaviour change). On Cockroach it retries on SQLSTATE `40001 RETRY_SERIALIZABLE` with exponential backoff up to `MAX_ATTEMPTS = 10` total executions (base 50ms, capped at 2s). Reuse this helper rather than rolling your own retry loop.

Conflict resolution emits the standard `INSERT … ON CONFLICT (key) DO …` SQL on both dialects. Cockroach's native `UPSERT` is deliberately not used: it silently uses the primary key as the conflict arbiter regardless of any user-declared `conflict.key`, which would mask misconfiguration if a user pointed at a UNIQUE secondary index instead.

Cockroach storage migrations live in `migrations/storage-cockroachdb/` (byte-identical copies of `storage-postgres/`); `PgStorage::migrate()` branches on `self.dialect` between the two `sqlx::migrate!` paths.

## MongoDB helpers

Mongo has no SQL surface, so `commons-mongodb` ships its own helper set:

- **`commons-mongodb::client`** — `mongodb::Client` builder + project-wide pool/timeout settings, reusing `commons::pool_timeouts`. Mongo has no per-statement timeout; the runner's per-call `tokio::time::timeout` covers that.
- **`commons-mongodb::identifier`** — gates database / collection names on the same character class as SQL identifiers.
- **`commons-mongodb::path`** — read / write nested BSON via `core::mapping::FieldPath`. `set` creates missing intermediate documents.
- **`commons-mongodb::bson_value`** — bidirectional codec between BSON and the canonical `Value`/`DataType`. ObjectId → `Bytes(12)`; BSON Date → `Timestamp` (UTC, sub-ms truncation documented inline); Decimal128 → `Decimal`; Document/Array → `Json`; Binary(uuid subtype) → `Uuid`. Unrepresentable BSON variants (regex, JS code, MinKey/MaxKey, …) error rather than silently dropping data.
- **`commons-mongodb::infer`** — sample-based schema inference. Folds per-field types; widens `int32 + int64` → `Int64`, `int + float` → `Float64`.
- **`commons-mongodb::sampling`** — `sample_documents` / `describe_collection_schema` / `rows_from_documents`. Shared between the `mongodb` and `mongo-cdc` sources. New mongo-shaped sources should call these instead of duplicating `$sample` aggregation pipelines.

## Type model and the N+N matrix

Canonical types are the only pivot — connectors do NOT introduce parallel enums. Each connector maps `native → DataType` on read and `DataType → native` on write.

- **`core::types::{DataType, Value}`** — the type enums. `Text`/`Bytes` carry `size: Option<u32>` (None = unbounded). `BigInt { width }` covers integer-only `numeric(p, 0)` (carrying `num_bigint::BigInt`); `Decimal { precision, scale }` covers fractional `numeric(p, s>0)` (carrying `bigdecimal::BigDecimal`). Unsigned variants exist solely for MySQL/MariaDB UNSIGNED columns — pg never produces them.
- **`core::types::matrix`** — validation-time width check. `is_compatible` is the lossless matrix; `is_compatible_with_truncate` widens to admit narrowing arms when a mapping has `truncate=true`. Reverse paths (`BigInt/Decimal → Int*`, `Float ↔ BigInt/Decimal`) are deliberately rejected by the lossless matrix.
- **`core::types::convert`** — runtime per-cell dispatcher (`convert(value, src, dst, &ctx)`). Identity / pure-widening pairs return the value unchanged. Connectors must NOT implement these conversions themselves; the runner builds a `ConversionPlan` per column and dispatches via `convert`.
- **`core::types::default_value::parse`** — parses a TOML default literal against the sink `DataType`. Bytes columns require a typed prefix (`hex:` / `base64:` / `utf8:` / `bin:`).
- **`FlowState::conversions`** is populated in `validation::pipeline::validate`. The runner skips identity plans and dispatches the rest via `convert`.

## Validation pipeline

Two stages: `assemble` (no I/O) → `validate` (probes, schema introspection, matrix, sampling). The pipeline groups assembled flows by `Source::name()` and runs them through `futures::join_all` — one async worker per source, sequential within. The CLI prints `running validation in {N} workers` at start. Output is sorted back into config order so error reporting is deterministic.

The flow-level `[flow.<name>.validation]` block exposes four toggles: `access`, `fields`, `inserts`, `sampling`. The first three default `true` and gate the access probes / matrix / sink write probe respectively. `sampling` follows the per-backend `SourceFactory::sampling_default()` — Mongo enables it (size 100), SQL keeps it disabled.

`Sink::schemaless()` is `true` for Mongo. The pipeline then derives the sink schema from the source's declared types, skipping the matrix narrowing check.

`Source::sample` drives `read_batch` with `limit=n` (validates the cursor SQL). `Source::sample_fresh` is an optional companion override for backends with random-access — Mongo overrides via `$sample`. Sampling-validation runs both and unions the rows before exercising the conversion plan.

`core::mapping::FieldPath` — `parse(&str)` produces a validated dot-notation path. SQL connectors reject `is_nested()` paths; Mongo accepts them.

## Conflict resolution

Optional `[flow.<name>.conflict]` block. `core::config::conflict::{ConflictConfig, ConflictStrategy}`. Without it, sinks do plain `INSERT` / `insertMany`. With it:

- pg → `ON CONFLICT (key) DO NOTHING|UPDATE SET …=EXCLUDED.…`
- mysql → `INSERT IGNORE` / `ON DUPLICATE KEY UPDATE … = VALUES(…)` (legacy form, MariaDB-compatible)
- mongo → `insertMany(ordered=false)` swallowing E11000 duplicates / per-row `replaceOne(upsert=true)` fired in parallel via `join_all`. Single-key `["_id"]` takes a fast path that skips the FieldPath round-trip.

## Interval parsing, secrets, config

- **`core::config::interval`** — parses `1s`, `1h30m`, `PT1H5S`, etc. into `Duration`. Used for `CursorConfig::interval` and `query-timeout`.
- **`core::config::env_expand`** — runs on raw TOML before parsing. Resolves `${VAR}` via env → `[secrets]` map → default → error. `std::env::var(...)` in connectors is forbidden.
- **`core::config::loader::load`** — entry point. Enforces 16 MiB file cap, no absolute-path includes, symlink-loop dedupe, `${VAR}` expansion, and structural validation (`batch_limit ≥ 1`, `batch_limit × mapping_cols ≤ 60_000`, cursor fields ⊆ mapping, `conflict.key` ⊆ mapping).

## Testing

- **`commons-testing::pg::pg_pool`** / **`mysql::mysql_pool`** / **`mariadb::mariadb_pool`** / **`mongo::mongo_pool`** — sandboxed handles. Honour `AIR_ELT_TEST_*_URL` or auto-detect podman/docker. Drop tears down the sandbox.
- `[dev-dependencies]` only — testcontainers must not ship in release builds.
- Database mocks are forbidden. **`mockall`** (dev-dep) is used only for runner-logic unit tests via `#[cfg_attr(test, mockall::automock)]` on `core::traits`.
- E2e suites: each connector owns its own. Cross-vendor flows are exercised by a *small fixed* sample, not an N×N matrix.

## Traits, runtime, registration

- **`core::traits::{Source, Sink, Storage}`** — `#[async_trait]`, object-safe. `Source::name()` is required (used by the validation pipeline to group flows by source pool).
- **`Source/Sink/Storage::cancel_safe() -> bool`** — default `true`. The runner's `run_op` consults it to pick a strategy: `true` → `tokio::time::timeout` + `select!` (cheap, sqlx connectors); `false` → `tokio::spawn` + detach so the underlying driver future is never dropped mid-await (the `mongodb` 3.x crate is not cancellation-safe). Mongo source / sink / storage override to `false`. Add the override on any future connector whose driver doesn't tolerate `Drop` mid-flight.
- Shared types: `Batch`, `Row`, `ReadSpec`, `WriteSpec`, `WriteReport`, `CursorState`. Connectors must not define their own.
- Factories are `#[async_trait]` in `core::registry`, registered as zero-sized structs in `app::registry::build_registry`. Do not construct connectors directly from flow code.
- **`core::flow::engine::FlowEngine`** — spawns one `FlowRunner` per flow. Each runner wraps every DB call via `run_op` (cancel-safe path = `tokio::time::timeout` + `select!`; cancel-unsafe path = `tokio::spawn` + detach). `query_timeout` on `FlowConfig` overrides the 30s default.
- **`Row.op: RowOp { Upsert, Delete }`** lives on every row. Pull-based sources always emit `Upsert` (the constructor `Row::upsert` handles that). CDC sources emit a mix of `Upsert` and `Delete`. All three sinks split the batch and apply **upsert → delete** order so `insert(k) → delete(k)` within one batch lands as "absent" in the sink.
- **`Storage::{load,save}_resume_token`** — the persistence path for CDC sources. Distinct from `{load,save}_cursor`: column cursors live in `air_elt_cursors`, resume tokens live in `air_elt_resume_tokens`. The runner picks between the two via `AssembledFlow.cursor_persistence: CursorPersistence::{ColumnCursor, ResumeToken}`, populated in `validation::pipeline::assemble` from the source's `kind`.
- **`core::flow::runner::dedup_cdc_batch`** — CDC-only batch compaction. Reverse-walks the rows and keeps only the last op per `conflict.key` fingerprint. Built on `write_value_key` (a direct binary encoder per `Value` variant — sidesteps Hash/Eq problems with floats and Json). Short-circuits if no `Delete` is present (the common upsert-only path stays allocation-free).

## Errors

Dedicated variants — use the right one instead of `RuntimeError::Other`. Wrap third-party errors with `RuntimeError::backend(err)` to preserve the `source` chain. Notable: `ValidationError::{NullabilityMismatch, DuplicateSinkField, SamplingFailed, MissingField, AccessFailed}`, `TypeError::NullSinkColumn`, `ConfigError::{UnresolvedReference, ConfigTooLarge, AbsoluteIncludeNotAllowed, Invalid}`.

## After changes

- Add a line to this file if you introduced a new utility others must use.
