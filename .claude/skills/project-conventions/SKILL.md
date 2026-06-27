---
name: project-conventions
description: Mandatory shared utilities and patterns for Air Elt — load before changing any Rust code so you use the right crate helpers instead of writing ad-hoc alternatives. Covers logging, SQL identifier escaping, value binding, config loading, secret resolution, type model, testing, factory wiring, and error types. Update this file whenever a new cross-crate utility is introduced.
user-invocable: false
---

# Project conventions — mandatory utilities

Before editing Rust code, check this list. If a utility exists for your need, use it — **do not reimplement**. Method signatures are not duplicated here; this is a "where to look" map. Add a line when you introduce a new cross-crate helper.

## Commons isolation

Foundation crates carry the no-internal-dep rule (no dependency on any other `air-elt-*` crate):

- `air-elt-commons` (`crates/commons/lib`) — utility helpers (`tracing_init`, `identifier`, `pool_timeouts`, `pool_settings`, `bool_flag`, `interval`).
- `air-elt-types` (`crates/types`) — canonical type model (`DataType`, `Value`, `Key`, conversion matrix, value comparison (`compare_values`, `values_equal`), JSON encoder, `DynType` / `DynValue` traits, `JsonEncodeError`).
- `air-elt-commons-caching` (`crates/commons/caching`) — `FifoCache<K, V>`: thread-safe bounded cache with FIFO eviction, cheap-to-clone (`Arc`-shared store). `new(cap)` (`cap == 0` = pass-through), `get_or_try_insert_with(key, build)`. FIFO, not LRU — hits take only a shared read lock. For caching compiled artifacts keyed by string (regex, JSON-path); store `Arc<T>` values.
- `air-elt-commons-arena` (`crates/commons/arena`) — `Arena<T>`: append-only `u16`-indexed arena for compact execution-order layout. `alloc -> ArenaRef<T>`, `open_slice` + `ArenaSlice::push` (Go-`append` style, grows only at the tail else errors — never relocates), `get`/`slice`. Type-tagged handles (`ArenaRef<T>` / `ArenaSlice<T>`) so a handle from one arena cannot index another.

Both new crates are generic and dependency-free; classified Foundation in the self-lint `CLASSIFICATION_RULES` so the `expr/*` crates may depend on them.

**All type casts and value comparisons belong in `air-elt-types`**, not in expression or connector code. Expression functions and connectors call `air_elt_types::compare_values` / `air_elt_types::convert` instead of hand-rolling match arms. Connector-local custom types implement `DynValue::partial_cmp` and `DynValue::is_equal` for ordering/equality of opaque values.

**`Value::variant_name() -> String`** is the canonical runtime type-name helper. It is **custom-aware** — a `Value::Custom` returns its `DynType::kind()` (e.g. `mongodb.object_id`), not a flat `"Custom"`. Use it instead of re-deriving a per-`Value`-variant name match; the expression `typeof` builtin and the redis sink both delegate to it. (Clickhouse's `value_variant` stays separate: its `got: &'static str` error field couples 23 call sites, so migrating costs more than the duplication saves.)

Both **MUST NOT depend on any other `air-elt-*` crate**. Direction of dependency is the inverse: `core` (and connectors) depend on both; never the other way around. If a type wants to bridge a foundational crate and `core` (e.g. `impl From<IdentifierError> for RuntimeError`), the impl belongs in `core` — `core` is allowed to know about commons and types. The `commons-pg` / `commons-mysql` / `commons-mongodb` / `commons-clickhouse` / `commons-questdb` crates legitimately depend on both `core` and the two foundational crates; `commons-lib` and `air-elt-types` do not.

Backend-specific custom `DynType` / `DynValue` impls (`mongodb.object_id`, `postgresql.hll`, `postgresql.inet`, `clickhouse.aggregate.*`, …) live in `commons-{backend}`, never in `air-elt-types` — they carry driver deps (sqlx, bson, reqwest, …) that the neutral types crate cannot pull in.

## Config naming

TOML keys use **kebab-case** for multi-word fields (`batch-limit`, `max-connections`). Structs carry `#[serde(rename_all = "kebab-case")]`. **No future-proofing fields** — every config field must be consumed by the implementation that ships with it.

## Logging

Initialise the subscriber once via `air_elt_commons::tracing_init` in `app::main`. The function returns an `Option<tracing_appender::non_blocking::WorkerGuard>` (`#[must_use]`) — bind it in `main` (`let _g = tracing_init::init();`) so the async background worker drains on shutdown. Env knobs: `AIR_ELT_LOG` / `RUST_LOG` (level), `AIR_ELT_SYNC_LOGGING` (opt-out of async writes), `AIR_ELT_JSON_LOGGING` (emit JSON). Style rules (structured fields, no instrument, no println) live in `rust-guidelines`.

## Boolean env / string flags

`air_elt_commons::bool_flag` — `parse(&str) -> Option<bool>` (accepts `true/1/t/y/yes` and `false/0/f/n/no`, case- and whitespace-insensitive) and `from_env(key, default)`. Use it for any string-typed boolean knob instead of hand-rolling `.eq_ignore_ascii_case("true")` per call site.

## SQL helpers

All dynamic SQL identifiers go through per-backend helpers — raw `format!` quoting is forbidden. `air_elt_commons::identifier` (validation + `IdentifierError`), `commons_pg`/`commons_mysql::identifier` (quoting). Native↔canonical type mapping in `commons-{pg,mysql}::{pg,mysql}_type`; value binding via `null_bind` / `sink_bind::bind_value_separated`; pools via `commons-{pg,mysql}::pool` (+ `pool_timeouts`); schema via `schema::fetch_schema`. Bind values with `$N` / `push_bind`, never interpolate. The Postgres crates also serve CockroachDB via a `#[serde(skip)]` `Dialect` flag — `with_serialization_retry` wraps every write, `Xml` is rejected, migrations are separate.

Full helper inventory, type quirks (`timestamptz`-only, `tinyint(1)`→Bool), pool defaults, and the Cockroach/`Dialect` specifics: [references/sql-helpers.md](references/sql-helpers.md).

## MongoDB helpers

Mongo has no SQL surface, so `commons-mongodb` owns its own set: `client` (builder + pool/timeouts), `identifier`, `path` (nested BSON via `FieldPath`), `bson_value` (BSON↔`Value` codec — unrepresentable variants error, never drop), `infer` + `sampling` (`$sample`-based schema, shared by `mongodb`/`mongo-cdc`), `key_bson::KeyBson` (total `Eq`/`Hash` for `_id` dedup), and `task::detached` (cancel-safety for the non-cancel-safe `mongodb` 3.x driver). Reuse these instead of duplicating pipelines.

Full helper inventory and BSON↔canonical type mapping: [references/mongodb-helpers.md](references/mongodb-helpers.md).

## ClickHouse helpers

ClickHouse is sink-only today; `commons-clickhouse` carries the shared helpers — reuse them, don't roll up ad-hoc HTTP / type-parsing. `client` (`reqwest` + `ChClientConfig`; we avoid the `clickhouse` crate because its typed `Row` API doesn't fit dynamic `Vec<Value>`), `identifier` (backtick), `ch_type_parser` (`system.columns.type` → `ParsedType`), `schema::fetch_schema`, `row_binary::encode_value` (RowBinary cell encoder), and the `types` `DynType`/`DynValue` registry for CH-specific columns (`fixed_string`, `enum8/16`, `int128/256`, `aggregate_state`; IPv4/IPv6 are canonical).

Full client/encoder/registry detail and the shipped `clickhouse.*` custom `kind` list: [references/clickhouse-helpers.md](references/clickhouse-helpers.md).

## Sink::supports_deletes (append-only ingest)

`Sink::supports_deletes() -> bool` (default `true`). ClickHouse and QuestDB sinks override to `false`. Three behavioural consequences:

1. **Sink self-filter (authoritative).** A sink that returns `false` from `supports_deletes()` MUST drop `RowOp::Delete` rows itself inside `write_batch` and return a `WriteReport` containing the count of upsert rows actually written. The runner no longer pre-filters — it ships the whole batch and lets the sink decide. The runner still advances the cursor on every successful `write_batch`, so an all-delete batch lands as `rows_written = 0` and the cursor moves past the dropped events. ClickHouse filters via `batch.rows.iter().filter(|r| r.op == RowOp::Upsert)`; QuestDB filters inside `pg_writer::write`.
2. **Validation pipeline**: `validate_delete_access` is gated on `source.emits_deletes() && conflict.is_some() && sink.supports_deletes()` — the probe is skipped against append-only sinks regardless of source/conflict shape.
3. **Assemble**: the otherwise-mandatory `[flow.<name>.conflict]` block for CDC sources (`mongo-cdc`) becomes optional when the sink declares no-delete. Append-only ingest: every CDC event lands as a plain INSERT.

Future no-delete sinks (e.g. an append-only event-store backend) MUST repeat the self-filter pattern — the runner does not assist. A regression test that feeds a Delete-only batch through the sink and asserts a clean `WriteReport { rows_written: 0 }` is the project convention.

## Type model and the N+N matrix

Canonical types are the only pivot — connectors map `native → DataType` on read and `DataType → native` on write. Compatibility is checked at validation time (`core::types::matrix::is_compatible` lossless, `is_compatible_with_truncate` for narrowing arms); per-cell conversion runs at row time inside the Transform layer (`core::types::convert`). Backend-specific types live behind `DataType::Custom(Box<dyn DynType>)` in `commons-{backend}/src/types/`, never in `core`. Sampled schemas (Mongo today) are a validation-time artifact only — the static `ColumnConversionPlan` MUST NOT be load-bearing for schemaless sources; `Source::schemaless() == true` switches the compiled Transform onto a dynamic-source `Convert` op that resolves the source `DataType` per cell. Full reference (every type, matrix arms, custom-kind requirements, sampled-schema escape hatches): [references/type-model.md](references/type-model.md).

## Key newtype (`air_elt_types::Key`)

Hashable, totally-ordered projection of `Value` for switch dispatch, batch dedup, and cursor comparison. `Value` has cross-numeric `PartialEq`/`PartialOrd` but does NOT implement `Hash` or `Eq` — for `HashMap`/`HashSet` usage, project through `Key`. Rejects Null/Json/Object; accepts cursor-compatible Custom. Canonicalises small ints → Int64, Float32 → Float64 on construction so cross-width values hash identically. The hash is deliberately **coarse**: cross-numeric-equal values (`Int64(1)` / `Float64(1.0)`, `Int64(n)` / `BigInt(n)`) hash by their `f64` value, and `Ipv4` / mapped-`Ipv6` by the v6 form, so values that `Eq` considers equal always share a bucket. `KeyBson` stays separate for BSON-layer dedup (raw `bson::Bson`, always `_id`).

### Property tests for key types (mandatory)
Any type used as a `HashMap`/`HashSet`/dispatch key (`Key`, `KeyBson`, and any new one) MUST carry property tests asserting the contracts, because a violation is a *silent* lookup miss, not a crash:
1. **`Eq` implies `Hash`**: for arbitrary pairs, `a == b` implies `hash(a) == hash(b)`. (The converse is not required — hash collisions are allowed and resolved by `Eq`, so a coarse hash is fine; an `Eq` that calls two values equal while `Hash` separates them is the bug.)
2. **`Ord` consistent with `Eq`**: `cmp(a, b) == Equal` iff `a == b`; plus reflexivity, antisymmetry, transitivity.
3. **Corner cases** the random strategy must cover (or add as explicit cases): `NaN`, `±0.0`, cross-numeric equals (`Int64`/`Float64`/`BigInt` of the same value), large integers near `2^53` where `int → f64` is lossy, and any cross-type equality the type allows (e.g. `Ipv4` ↔ IPv4-mapped `Ipv6`). See `crates/types/src/key.rs` tests for the reference set.

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

Two stages: `assemble` (no I/O — build components, derive specs, compile the Transform) → `validate` (probes, schema introspection, matrix, sampling). Flows run concurrently via `for_each_concurrent`; backend contention is bounded by per-component `tokio::sync::Semaphore`s (one per declared source/sink/storage, sized to `max-connections`). **Permits are strictly local** — held only across the single I/O call, never across two backend calls or an unrelated `await` — so deadlock between flows is structurally impossible. Probes retry transient `Backend` errors (`retry_transient`); `max-connections = 0` is rejected up front. The `[flow.<name>.validation]` toggles (`access`/`fields`/`inserts`/`sampling`) gate each stage; `Sink::schemaless()` (Mongo) derives the sink schema from the source; `Source::sample` drives sampling-validation (CDC sources override to `$sample`). `core::mapping::FieldPath` validates dot-paths (SQL rejects nested, Mongo accepts).

Full semaphore/locking model, retry/backoff timings, toggle defaults, and sampling specifics: [references/validation-pipeline.md](references/validation-pipeline.md).

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
- **Walking `OptExpr` trees in the optimizer:** do not hand-roll an exhaustive child-recursion match. `crates/expr/optimize/src/util/visit.rs` owns the only two exhaustive child enumerations, all `ControlFlow`-based (callback returns `Continue(())` to keep walking, `Break(value)` to stop; the visitor propagates the break to the caller). Pick by recursion needs: `for_each_recursive{,_mut}` visits the whole subtree pre-order — use it for flat node predicates (any-scan via `.is_break()`: `contains_block`, `can_fail`; all-predicate via `.is_continue()`: `is_pure`; side-effect walks discard the result: `collect`, `count_fields`, `rewrite_fields`). `for_each_child{,_mut}` visits direct children only — use it when your pass drives its own recursion order or handles some variants itself (`constant_inliner`, `prune_blocks`). Walks that consume/rebuild nodes or do per-variant semantic work (rewrite drivers, type synthesis, compaction) stay explicit.
- Public API in `expr/parse` and `expr/runtime` is intentionally minimal. Do not add public methods without justification.

## After changes

- Add a line to this file if you introduced a new utility others must use.
