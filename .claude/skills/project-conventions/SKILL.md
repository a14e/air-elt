---
name: project-conventions
description: Mandatory shared utilities and patterns for Air Elt — load before changing any Rust code so you use the right crate helpers instead of writing ad-hoc alternatives. Covers logging, SQL identifier escaping, value binding, config loading, secret resolution, type model, testing, factory wiring, and error types. Update this file whenever a new cross-crate utility is introduced.
user-invocable: false
---

# Project conventions — mandatory utilities

Before editing Rust code, check this list. If a utility exists for your need, use it — **do not reimplement**. Method signatures are not duplicated here; this is a "where to look" map. Add a line when you introduce a new cross-crate helper.

## Commons isolation

Two foundational crates carry the no-internal-dep rule:

- `air-elt-commons` (`crates/commons/lib`) — utility helpers (`tracing_init`, `identifier`, `pool_timeouts`, `pool_settings`, `bool_flag`, `interval`).
- `air-elt-types` (`crates/types`) — canonical type model (`DataType`, `Value`, `Key`, conversion matrix, value comparison (`compare_values`, `values_equal`), JSON encoder, `DynType` / `DynValue` traits, `JsonEncodeError`).

**All type casts and value comparisons belong in `air-elt-types`**, not in expression or connector code. Expression functions and connectors call `air_elt_types::compare_values` / `air_elt_types::convert` instead of hand-rolling match arms. Connector-local custom types implement `DynValue::partial_cmp` and `DynValue::is_equal` for ordering/equality of opaque values.

Both **MUST NOT depend on any other `air-elt-*` crate**. Direction of dependency is the inverse: `core` (and connectors) depend on both; never the other way around. If a type wants to bridge a foundational crate and `core` (e.g. `impl From<IdentifierError> for RuntimeError`), the impl belongs in `core` — `core` is allowed to know about commons and types. The `commons-pg` / `commons-mysql` / `commons-mongodb` / `commons-clickhouse` / `commons-questdb` crates legitimately depend on both `core` and the two foundational crates; `commons-lib` and `air-elt-types` do not.

Backend-specific custom `DynType` / `DynValue` impls (`mongodb.object_id`, `postgresql.hll`, `postgresql.inet`, `clickhouse.aggregate.*`, …) live in `commons-{backend}`, never in `air-elt-types` — they carry driver deps (sqlx, bson, reqwest, …) that the neutral types crate cannot pull in.

## Config naming

TOML keys use **kebab-case** for multi-word fields (`batch-limit`, `max-connections`). Structs carry `#[serde(rename_all = "kebab-case")]`. **No future-proofing fields** — every config field must be consumed by the implementation that ships with it.

## Logging

Initialise the subscriber once via `air_elt_commons::tracing_init` in `app::main`. The function returns an `Option<tracing_appender::non_blocking::WorkerGuard>` (`#[must_use]`) — bind it in `main` (`let _g = tracing_init::init();`) so the async background worker drains on shutdown. Env knobs: `AIR_ELT_LOG` / `RUST_LOG` (level), `AIR_ELT_SYNC_LOGGING` (opt-out of async writes), `AIR_ELT_JSON_LOGGING` (emit JSON). Style rules (structured fields, no instrument, no println) live in `rust-guidelines`.

## Boolean env / string flags

`air_elt_commons::bool_flag` — `parse(&str) -> Option<bool>` (accepts `true/1/t/y/yes` and `false/0/f/n/no`, case- and whitespace-insensitive) and `from_env(key, default)`. Use it for any string-typed boolean knob instead of hand-rolling `.eq_ignore_ascii_case("true")` per call site.

## SQL helpers

All dynamic SQL identifiers must go through these helpers. Raw `format!` quoting is forbidden.

- **`air_elt_commons::identifier`** — db-agnostic validation primitives + `IdentifierError`.
- **`air_elt_commons_pg::identifier`** — pg quoting (`"`).
- **`air_elt_commons_mysql::identifier`** — mysql quoting (backtick).
- **`IdentifierError → RuntimeError`** via `impl From` in `core::error` — use `?` directly.

Source-side type resolution lives in `commons-pg::pg_type` / `commons-mysql::mysql_type` (native ↔ canonical `DataType`). Notable quirks: pg accepts `timestamptz` only (naive `timestamp` rejected); mysql `tinyint(1)` → `Bool`, other signed tinyints → `Int8` (was `Int16` before AIR-22), `datetime` rejected (only `timestamp` accepted, UTC).

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

## ClickHouse helpers

ClickHouse is sink-only today; `commons-clickhouse` carries the helpers shared with any future CH source. Reuse these — do not roll up ad-hoc HTTP / type-parsing per call site.

- **`commons-clickhouse::client`** — `reqwest::Client` wrapper plus `ChClientConfig` (URL, database, `user`, `password`, `PoolSettings`). `user` and `password` are required strings — use `""` for the authless variant on CH instances with `<networks>` open for the `default` user. Both auth headers (`X-ClickHouse-User` / `X-ClickHouse-Key`) are always emitted regardless of value; no "skip header when empty" branching. `ping()`, `query_text()`, `insert_row_binary()`. We use `reqwest` directly rather than the `clickhouse` 0.13 crate because that crate's typed `Client::insert::<T: Row>` API doesn't fit dynamic `Vec<Value>` batches.
- **`commons-clickhouse::identifier`** — backtick quoting (CH shares MySQL's backtick syntax). `quote_ident`, `quote_qualified`, `quote_columns`, `split_qualified`.
- **`commons-clickhouse::ch_type_parser`** — recursive parser for `system.columns.type` strings. Returns `ParsedType { data_type, nullable }`. `Nullable(T)` strips onto the `nullable` flag; `LowCardinality(T)` strips transparently. Composite shapes (`Array`, `Tuple`, `Map`, `Nested`, geo) map onto `DataType::Json`.
- **`commons-clickhouse::schema`** — `fetch_schema(client, table)` runs `SELECT name, type FROM system.columns … FORMAT JSON` and folds the result into a canonical `Schema`.
- **`commons-clickhouse::row_binary`** — `encode_value(out, &Field, &Value)` writes one column-cell into a `RowBinary` byte buffer. Handles `Nullable` flag bytes, UTF-8 string LEB128 length prefix, CH's mixed-endian UUID layout, Date as `u16` days, DateTime as `u32` seconds (UTC, no TZ), `Decimal` as fixed-width signed LE (width by precision: ≤9=i32, ≤18=i64, ≤38=i128, ≤76=i256).
- **`commons-clickhouse::types`** — the CH `DynType`/`DynValue` registry:
  - `aggregate_state` — `ChAggregateStateType { fn_name, arg_types, simple }` + opaque-bytes `ChAggregateStateValue`. `kind()` is `clickhouse.aggregate.<snake_fn>` (leak-interned at first observation per process — bounded by user-declared columns).
  - IPv4 / IPv6 are canonical (`DataType::Ipv4` / `DataType::Ipv6`, `Value::Ipv4(Ipv4Addr)` / `Value::Ipv6(Ipv6Addr)`); CH `IPv4` columns encode as LE u32 and `IPv6` as 16 BE octets inside `commons-clickhouse::row_binary`.
  - `fixed_string` — `ChFixedStringType { size }` + bytes carrier. Cross-canonical to/from `Bytes(N)`.
  - `enum_` — `ChEnum8Type` / `ChEnum16Type` (variants table) + `ChEnumValue { name }`. Cross-canonical to/from `Text` (variant name).
  - `int128` — `ChInt128Type` / `ChUInt128Type` + `ChInt128Value(i128)` / `ChUInt128Value(u128)`. 16-byte LE. Cross-canonical to/from `BigInt`.
  - `int256` — `ChInt256Type` / `ChUInt256Type` + `ChInt256Value { le_bytes: [u8; 32] }` / `ChUInt256Value { le_bytes: [u8; 32] }`. 32-byte LE two's-complement. Cross-canonical to/from `BigInt`. Helpers: `bigint_to_le32`, `le32_to_bigint`, `biguint_to_le32`.

**Custom `kind` values shipped**: `clickhouse.fixed_string`, `clickhouse.enum8`, `clickhouse.enum16`, `clickhouse.int128`, `clickhouse.uint128`, `clickhouse.int256`, `clickhouse.uint256`, `clickhouse.aggregate.<fn>` (e.g. `clickhouse.aggregate.quantiles_t_digest`, `clickhouse.aggregate.quantiles_d_d_sketch`). IPv4 / IPv6 used to live here as `clickhouse.ipv4` / `clickhouse.ipv6`; they have been promoted to canonical `DataType::Ipv4` / `DataType::Ipv6` (AIR-88).

## Sink::supports_deletes (append-only ingest)

`Sink::supports_deletes() -> bool` (default `true`). ClickHouse and QuestDB sinks override to `false`. Three behavioural consequences:

1. **Sink self-filter (authoritative).** A sink that returns `false` from `supports_deletes()` MUST drop `RowOp::Delete` rows itself inside `write_batch` and return a `WriteReport` containing the count of upsert rows actually written. The runner no longer pre-filters — it ships the whole batch and lets the sink decide. The runner still advances the cursor on every successful `write_batch`, so an all-delete batch lands as `rows_written = 0` and the cursor moves past the dropped events. ClickHouse filters via `batch.rows.iter().filter(|r| r.op == RowOp::Upsert)`; QuestDB filters inside `pg_writer::write`.
2. **Validation pipeline**: `validate_delete_access` is gated on `source.emits_deletes() && conflict.is_some() && sink.supports_deletes()` — the probe is skipped against append-only sinks regardless of source/conflict shape.
3. **Assemble**: the otherwise-mandatory `[flow.<name>.conflict]` block for CDC sources (`mongo-cdc`) becomes optional when the sink declares no-delete. Append-only ingest: every CDC event lands as a plain INSERT.

Future no-delete sinks (e.g. an append-only event-store backend) MUST repeat the self-filter pattern — the runner does not assist. A regression test that feeds a Delete-only batch through the sink and asserts a clean `WriteReport { rows_written: 0 }` is the project convention.

## Type model and the N+N matrix

Canonical types are the only pivot — connectors map `native → DataType` on read and `DataType → native` on write. Compatibility is checked at validation time (`core::types::matrix::is_compatible` lossless, `is_compatible_with_truncate` for narrowing arms); per-cell conversion runs at row time inside the Transform layer (`core::types::convert`). Backend-specific types live behind `DataType::Custom(Box<dyn DynType>)` in `commons-{backend}/src/types/`, never in `core`. Sampled schemas (Mongo today) are a validation-time artifact only — the static `ColumnConversionPlan` MUST NOT be load-bearing for schemaless sources; `Source::schemaless() == true` switches the compiled Transform onto a dynamic-source `Convert` op that resolves the source `DataType` per cell. Full reference (every type, matrix arms, custom-kind requirements, sampled-schema escape hatches): [references/type-model.md](references/type-model.md).

## Key newtype (`air_elt_types::Key`)

Hashable, totally-ordered projection of `Value` for switch dispatch, batch dedup, and cursor comparison. `Value` has cross-numeric `PartialEq`/`PartialOrd` but does NOT implement `Hash` or `Eq` — for `HashMap`/`HashSet` usage, project through `Key`. Rejects Null/Json/Object; accepts cursor-compatible Custom. Canonicalises small ints → Int64, Float32 → Float64 on construction so cross-width values hash identically. `KeyBson` stays separate for BSON-layer dedup (raw `bson::Bson`, always `_id`).

## Schema on context

Each source / sink ctx struct (`PgSourceCtx`, `MySqlSinkCtx`, etc.) carries its schema as a plain field, populated once in `build_context`:

- SQL ctxs: `pub schema: Schema`.
- Mongo ctxs: `pub schema: Option<Schema>` — the schema is sample-derived and may be absent when the collection is empty or unreachable.

Generic access goes through the `SchemaProvider` trait + `as_schema_provider()` helper on `dyn SourceCtx` / `dyn SinkCtx`. **Never** wrap the schema in `RwLock<HashMap<String, Schema>>` or similar — caching primitives are forbidden here. The ctx is rebuilt as a unit, not refreshed in place.

**Reset on backend error**: the runner drops the ctx Arc on `RuntimeError::Backend` *before* the backoff sleep; the next tick calls `build_context` again, which re-introspects the schema. Per-row data errors (`RuntimeError::JsonEncode`, `Type`) do NOT trigger ctx-drop — the connection is fine, the row isn't.

## Derived plans on FlowState

`FlowState` carries `derived: Option<DerivedPlans>` (a plain field, **not** behind a `Mutex` — `FlowState` is owned exclusively by a single `FlowRunner`; concurrent access is not part of the contract because each `tokio::spawn` moves a fresh `FlowState`). `DerivedPlans` holds:

- `transform: Transform` — the compiled per-flow Transform program (sequence of `TransformOp` lowered from the expanded mapping, with the identity short-circuit and absorb-when-last optimisation baked in),
- `read_spec_columns` / `write_spec_columns` (post-expansion, runner snapshots into `ReadSpec`/`WriteSpec` per tick).

Pure rebuild lives in `core::model::flow_state::build_derived_plans` (also used by validation pipeline at startup). Runner calls `state.invalidate_derived()` alongside ctx-drop and `state.rebuild_derived(...)` on the next tick after `build_context` populates fresh schemas — so a schema change between reconnects propagates through into a freshly compiled Transform.

## Transform layer

`core::transform` owns the only Row→Row machinery. The IR is closed:

```rust
pub enum TransformOp {
    Take { source_index: usize },                              // raw.values[i].take()
    Body,                                                       // raw.body.take()
    Convert { input: Box<TransformOp>, plan: ColumnConversionPlan },
}
```

`Transform::apply(raw: RawBatch) -> RuntimeResult<Batch>` runs the program per row. The compiler caches an identity short-circuit (every column is `Take{i}` for `i in 0..len`) — that path zero-copies the source values. The "absorb-when-last" optimisation moves the value out of the last `TransformOp::Take{i}` referencing source slot `i` (and likewise for the `Body` payload); earlier references clone.

Body construction for relational sources goes through **`air_elt_core::transform::build_body_json`** (relational sources call it when `ReadSpec.needs_body` is set). Mongo sources push `Value::Custom(BsonObjectValue(doc))` directly. The synthetic mapping target `core::mapping::expand::ROOT_BODY_TARGET = "_root"` is the lowered shape of mongo→mongo `["*"]` raw passthrough — it compiles to a single `TransformOp::Body`.

**Forbidden idiom**: `serde_json::to_value(&value)`. `Value`'s own `Serialize` emits the cursor-envelope `{type, value}` for storage, NOT the canonical wire format. Always go through `core::types::json_encode::value_to_json` (or `build_body_json`, which delegates to it).

## Validation pipeline

Two stages: `assemble` (no I/O) → `validate` (probes, schema introspection, matrix, sampling). All assembled flows are driven concurrently through `futures::stream::iter(...).for_each_concurrent(None, validate_flow)` — no per-source grouping. Backend contention is bounded purely by the per-component `tokio::sync::Semaphore`s built in `assemble`, sized to each backend's `max-connections`. The CLI prints `running validation for {N} flows; semaphores cap {source=K, sink=L, storage=M}` at start. Output is sorted back into config order so error reporting is deterministic.

### Concurrency: per-component semaphores

`assemble` builds one `tokio::sync::Semaphore` per declared `[[sources]]`/`[[sinks]]`/`[[storages]]` instance, with permit count = the component's `max-connections` (capped at `Semaphore::MAX_PERMITS`). Flows sharing a component share the same `Arc<Semaphore>`. Each flow gets a `FlowLockHandle` (`core::util::concurrency`) that exposes `acquire_source()` / `acquire_sink()` / `acquire_storage()` — one permit per component kind.

**Locks must be strictly local — held only across the single I/O call that touches the component, then released. Never hold a permit across an unrelated `await` or across two backend calls; that's a parasitic block on sibling flows that share the pool.** The runner enforces this by scoping each `acquire_*` to a tight `{ let _g = ...acquire_X().await?; <single call> }` block: `ensure_built` takes source for source `build_context`, releases, takes sink for sink `build_context`, releases; cursor load/save take storage; `read_batch` / `sample` take source; `write_batch` takes sink. Transform runs without any permit (pure compute). The validation pipeline mirrors this — each probe / schema fetch scopes its own permit.

Because no call site ever holds two permits at once, **deadlock between flows is structurally impossible** — there is no canonical lock order to maintain, no AB-BA hazard to defend against. A long PG read in one flow no longer blocks an unrelated CH write in another flow that happens to share the storage; the two only contend on permits they both actually use.

Access probes inside validation are additionally wrapped in `retry_transient` (`core::util::retry`): three attempts (50 ms → 250 ms → 1.25 s), retrying only `RuntimeError::Backend`; every other error is authoritative and fails immediately. The runtime tick has its own exponential backoff (`1s → 4× → 1h cap`) on `Err`; the inter-tick idle sleep and the backoff sleep both happen AFTER the tick returns, so they never hold any permit. `dry_run` governs *what* the tick does (`sample` vs `read_batch`, no-op sink write, skip cursor save) — not *whether* it acquires.

`max-connections = 0` is rejected at `build_*` time (`PoolSettingsError::ZeroMaxConnections`): a zero-permit semaphore would hang every flow forever. Operators see a config error before any I/O is attempted.

The flow-level `[flow.<name>.validation]` block exposes four toggles: `access`, `fields`, `inserts`, `sampling`. The first three default `true` and gate the access probes / matrix / sink write probe respectively. `sampling` follows the per-backend `SourceFactory::sampling_default()` — Mongo enables it (size 100), SQL keeps it disabled.

`Sink::schemaless()` is `true` for Mongo. The pipeline then derives the sink schema from the source's declared types, skipping the matrix narrowing check.

`Source::sample` is a single probe used by sampling-validation. The default delegates to `read_batch` with `spec.limit = n` and no cursor state — pull-based sources stay on the default so the probe exercises the same query the runner runs. CDC sources (`mongo-cdc`) override because their `read_batch` would block on the open change stream; the override aggregates `$sample` on the watched collection. Sampling-validation feeds the returned `RawBatch` through the compiled Transform.

`core::mapping::FieldPath` — `parse(&str)` produces a validated dot-notation path. SQL connectors reject `is_nested()` paths; Mongo accepts them.

## Conflict resolution

Optional `[flow.<name>.conflict]` block. `core::config::conflict::{ConflictConfig, ConflictStrategy}`. Without it, sinks do plain `INSERT` / `insertMany`. With it:

- pg → `ON CONFLICT (key) DO NOTHING|UPDATE SET …=EXCLUDED.…`
- mysql → `INSERT IGNORE` / `ON DUPLICATE KEY UPDATE … = VALUES(…)` (legacy form, MariaDB-compatible)
- mongo → `insertMany(ordered=false)` swallowing E11000 duplicates / per-row `replaceOne(upsert=true)` fired in parallel via `join_all`. Single-key `["_id"]` takes a fast path that skips the FieldPath round-trip.

## Interval parsing, secrets, config

- **`air_elt_commons::interval`** — **the canonical duration parser for the whole workspace.** Do NOT introduce a custom parser, regex, or `humantime`-style helper anywhere else; if your config field accepts a duration, route it through this module. Parses `1s`, `1h30m`, `PT1H5S`, ISO-8601 forms, etc. into `Duration`. Lives in the foundational commons crate so crates that can't depend on `core` (notably `air-elt-monitoring`) can reuse it without a workspace cycle. All callers import directly from `air_elt_commons::interval` (the `core::config::interval` re-export façade was removed). Used for `CursorConfig::interval`, `query-timeout`, the metrics summary window, every connector's pool timeouts, and the Mongo `operation-timeout`. Public API: `parse(&str) -> Result<Duration, _>`, `deserialize` / `serialize` / `to_iso` for serde plumbing, and `parse_allow_zero` / `deserialize_opt_allow_zero` for the zero-permitting variants used by fields whose `"0s"` value carries explicit meaning (today: `cursor.jitter`).
- **`core::config::env_expand`** — runs on raw TOML before parsing. Resolves `${VAR}` via env → `[secrets]` map → default → error. `std::env::var(...)` in connectors is forbidden.
- **`core::config::loader::load`** — entry point. Enforces 16 MiB file cap, no absolute-path includes, symlink-loop dedupe, `${VAR}` expansion, and structural validation (`batch_limit ≥ 1`, `batch_limit × mapping_cols ≤ 60_000`, cursor fields ⊆ mapping, `conflict.key` ⊆ mapping, `cursor.jitter ≤ cursor.interval` when set).
- **`CursorConfig::effective_jitter`** — resolves `cursor.jitter` to a concrete `Duration`. Defaults to `min(interval, 5min)` when the operator omits the field — the full interval, capped at five minutes. Explicit `"0s"` is honoured as "disable jitter". The runner sleeps a deterministic offset `ahash(flow.name) mod jitter` before the first tick (`FlowRunner::jitter_offset`, using `ahash::AHasher::default()` — the workspace's standard deterministic hasher), spreading concurrent flows across the cadence period. For typical scaffolds (1s interval, ≤5 max-connections per pool) this gives ≥0.5 ops/ms even at 500 flows fan-in to one sink. Set `cursor.jitter = "0s"` to disable.
- **Tick scheduling is fixed-rate, anchored to `UNIX_EPOCH`** (see `FlowRunner::next_tick_instant`). The flow's tick grid is the set `{ t : (t - offset_ns) mod interval == 0 }` where `offset_ns = hash(flow_name) mod min(jitter, interval)`. After each tick that returns an empty batch, the runner sleeps to the **next future** grid point — missed grid points (slow tick, long backoff, busy semaphore) are NEVER caught up. This keeps the per-flow jitter spread stable across the lifetime of the process and across restarts, and prevents back-to-back catchup bursts that would re-cluster flows after any slow phase. The math is a pure function and unit-tested independently of the tokio runtime.

## Testing

- **`commons-testing::pg::pg_pool`** / **`mysql::mysql_pool`** / **`mariadb::mariadb_pool`** / **`mongo::mongo_pool`** — sandboxed handles. Honour `AIR_ELT_TEST_*_URL` or auto-detect podman/docker. Drop tears down the sandbox.
- `[dev-dependencies]` only — testcontainers must not ship in release builds.
- Database mocks are forbidden. **`mockall`** (dev-dep) is used only for runner-logic unit tests via `#[cfg_attr(test, mockall::automock)]` on `core::traits`.
- E2e suites: each connector owns its own. Cross-vendor flows are exercised by a *small fixed* sample, not an N×N matrix.
- **Match test fixture size to `ReadSpec.limit`.** Sources with idle-drain semantics (CDC change streams in particular) only exit `read_batch` early when `events.len() >= spec.limit`; with fewer events the loop blocks on the underlying stream until `operation_timeout` (default 30s) fires. When writing a test, count the events your fixture produces and set `limit` to that exact number — or pad the fixture to match `limit`. Don't paper over the wait by lowering `operation_timeout` in tests; that hides the contract from production callers and invites flakes under CI jitter.
- Per-test test binaries are slow to build and link. Aggregate `tests/*.rs` files via a single `tests/all.rs` (`mod foo; mod bar;`) plus `autotests = false` + `[[test]] name = "all"` in `Cargo.toml`. New integration tests go as a `mod` inside `all.rs`, not as a new top-level file.

## Traits, runtime, registration

- **`core::traits::{Source, Sink, Storage}`** — `#[async_trait]`, object-safe. `Source::name()` is required (used by the validation pipeline to group flows by source pool).
- **Cancel-safety lives in the adapter, not the runner.** The runner wraps every call in `tokio::time::timeout` + a shutdown `select!`. Connectors whose driver tolerates `Drop` mid-await (sqlx, reqwest) do nothing extra. Connectors whose driver does not (the `mongodb` 3.x crate) wrap their driver calls in `air_elt_commons_mongodb::task::detached` — it spawns the work on the runtime so the driver future survives an outer drop.
- Shared types: `Batch`, `Row`, `ReadSpec`, `WriteSpec`, `WriteReport`, `CursorState`. Connectors must not define their own.
- Factories are `#[async_trait]` in `core::registry`, registered as zero-sized structs in `app::registry::build_registry`. Do not construct connectors directly from flow code.
- **`core::flow::engine::FlowEngine`** — spawns one `FlowRunner` per flow. Each runner wraps every DB call in `with_timeout` (`tokio::time::timeout` + shutdown `select!`); `query_timeout` on `FlowConfig` overrides the 30s default.
- **`Row.op: RowOp { Upsert, Delete }`** lives on every row. Pull-based sources always emit `Upsert` (the constructor `Row::upsert` handles that). CDC sources emit a mix of `Upsert` and `Delete`. All three sinks split the batch and apply **upsert → delete** order so `insert(k) → delete(k)` within one batch lands as "absent" in the sink.
- **`Storage::{load,save}_resume_token`** — the persistence path for CDC sources. Distinct from `{load,save}_cursor`: column cursors live in `air_elt_cursors`, resume tokens live in `air_elt_resume_tokens`. The runner picks between the two via `AssembledFlow.cursor_persistence: CursorPersistence::{ColumnCursor, ResumeToken}`, populated in `validation::pipeline::assemble` from the source's `kind`.
- **`commons-mongodb::key_bson::KeyBson`** — newtype around `bson::Bson` with total `Eq` + `Hash` (NaN==NaN, Null==Null; recursion through `Document`/`Array`). Used by the mongo-cdc source to dedup change-stream events by `_id` directly on the BSON value. CDC batch compaction now lives **inside** the source (mongo-cdc `read_batch` reverse-walks events, last-event-wins per `_id` before any post-image lookup). The core runner no longer carries a generic dedup pass — `Row::raw_key` / `dedup_key_indices` were removed with it.

## Errors

Dedicated variants — use the right one instead of `RuntimeError::Other`. Wrap third-party errors with `RuntimeError::backend(err)` to preserve the `source` chain. Notable: `ValidationError::{NullabilityMismatch, DuplicateSinkField, SamplingFailed, MissingField, AccessFailed, WildcardWithoutSchema, WildcardUniverseTooLarge, WildcardMissingNonNullableSource, CursorRequiresExplicitFields, ConflictKeyNotInMapping}`, `TypeError::NullSinkColumn`, `ConfigError::{UnresolvedReference, ConfigTooLarge, AbsoluteIncludeNotAllowed, Invalid}`, `RuntimeError::JsonEncode`, `JsonEncodeError::{Variant, DepthExceeded, CustomFailed}`.

## Metrics

`air-elt-monitoring` (`crates/monitoring`) is the single owner of Prometheus instrumentation. Every other crate reaches metrics through its `MonitoringManager` — direct `prometheus::*` calls outside `monitoring` are forbidden. Recorders are `Disabled | Enabled(Arc<Inner>)`; the disabled mode is the default and reduces every instrumented call site to an enum-discriminant load. The crate ships custom `Summary` (sliding-window DDSketch — `prometheus` 0.14 has none) and `TimeIntegratingGauge` (Kahan-summed `last_value * dt`, suffix `_seconds_integral` — read as a time-average via `rate(...)`). Air Elt's brief requires every long-running process to be observable: when you add a loop, retry, blocking acquire, user-visible failure surface, or latency-sensitive timed op, also add a metric for it. Full reference (recorder pattern, custom collectors, current metric inventory, naming): [references/metrics.md](references/metrics.md).

## Expression evaluation and runtime

- `ExpressionContext` now lives in `air_elt_expr_runtime::context`, not in `core::config::expression`.
- `ensure_sink_compatible` now lives in `air_elt_types::sink_compat`.
- `air_elt_expr_runtime::patcher::ConfigExprPatcher` — trie-based TOML tree patcher for evaluating expressions at specified paths.
- `air_elt_expr_runtime::evaluator::Evaluator` — standalone expression evaluator (replaces the removed `ExprValue.eval()`).
- `air_elt_expr_runtime::type_resolver::TypeResolver` — compile-time type resolution for expressions.
- Public API in `expr/parse` and `expr/runtime` is intentionally minimal. Do not add public methods without justification.

## After changes

- Add a line to this file if you introduced a new utility others must use.
