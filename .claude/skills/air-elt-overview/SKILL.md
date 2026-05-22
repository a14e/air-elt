---
name: air-elt-overview
description: Architecture and product overview of the Air Elt ELT service — features, tech stack, workspace layout, and config format. Load this before making non-trivial changes so you understand the product shape, crate boundaries, and the GitOps/declarative flow model.
user-invocable: false
---

# Air Elt — project overview

Air Elt is a Rust service for moving data between systems with **minimal transformation** (ELT, not ETL). Data is carried through as-is; only essential coercion happens at the edges.

## Operating philosophy

Validation prioritises **correctness**: every access probe, type check, and config constraint must pass before any data moves. Runtime prioritises **fault tolerance**: individual flow failures are logged and retried, never crashing the whole process.

- Flows are **declarative TOML or YAML** (GitOps). One file per flow.
- **Static SQL.** Statements are composed during config-init, never per-row.
- **Micro-batch + drain.** Sub-second batches with drain semantics (pull while full, sleep when empty). A nightly mode is available via a long interval.
- **Structured logs** via `tracing` only — no `println!`.
- **Processes must be observable.** Every long-running loop, retry, blocking acquire, user-visible failure, and latency-sensitive operation carries a Prometheus metric. Observability is a precondition for shipping a feature, not a follow-up. See `project-conventions::Metrics` for the recorder pattern and current inventory.
- **Minimal resources.** Run centralised, or place instances next to each data plane.

## Tech stack

Rust 1.95 stable (pinned via `rust-toolchain.toml`), tokio, async-trait, sqlx (postgres / mysql / migrate / chrono / uuid / json), mongodb 3.x driver, tracing, clap, mimalloc, thiserror.

## Workspace layout

```
air-elt/
├── rust-toolchain.toml
├── Cargo.toml                       # [workspace] + pinned [workspace.dependencies]
├── crates/
│   ├── app/                         # bin air-elt (CLI, mimalloc, tracing init, registry wiring)
│   ├── core/                        # traits, types, config, validation, flow runner
│   ├── commons/lib/                 # tracing-init, identifier, pool_timeouts (db-agnostic)
│   ├── commons/{pg,mysql,mongodb}/  # per-backend shared helpers
│   ├── commons/testing/             # PgTestHandle / MySqlTestHandle / MongoTestHandle
│   ├── sources/{postgres,mysql,mongodb}/    # connectors per backend
│   ├── sinks/{postgres,mysql,mongodb}/
│   └── storages/{postgres,mysql,mongodb}/   # storage + migrations dir per backend
├── migrations/storage-{postgres,mysql}/  # raw SQL for sqlx::migrate! (mongo storage has no migrations)
└── examples/{pg-to-pg,mysql-to-mysql,mongo-to-mongo}/   # usage examples
```

**Cross-dependencies between `sources/*`, `sinks/*`, `storages/*` are forbidden.** Each depends only on `core` (and optionally `commons`). Connectors are wired into the app via `core::registry::Registry`.

The `mysql` connector is exercised against both vanilla MySQL **and MariaDB** (10.7+ for native UUID, version-aware UPSERT for `VALUES()` legacy form). MariaDB is a *test target*, not a separately registered backend.

The `postgres` connector crates are also reused as the **CockroachDB** backend, registered under a separate factory key `cockroachdb` with `Dialect::Cockroach` (in `air-elt-commons-pg`) selecting the divergent code paths: automatic `40001` (`RETRY_SERIALIZABLE`) retry on writes via `air_elt_commons_pg::retry::with_serialization_retry`, upfront `XML`-type rejection at `validate_access`, and `set_locking(false)` on the migrator (Cockroach has no `pg_advisory_lock`). Conflict resolution stays on the standard `INSERT … ON CONFLICT` path on both engines — Cockroach's native `UPSERT` is deliberately not used because it ignores the user-declared `conflict.key` and silently uses the primary key as arbiter. Migrations for the cockroach storage live in `migrations/storage-cockroachdb/` (byte-identical to the postgres ones; `TEXT`/`JSONB`/`TIMESTAMPTZ` are all supported). For the Postgres dialect the new code paths are pure pass-throughs — behaviour is unchanged.

## Validation pipeline

Two stages. **Assemble** (no I/O): looks up components by name, builds them through factories, derives `ReadSpec`/`WriteSpec` and compiles the per-flow Transform program. **Validate** (I/O): runs access probes, schema introspection, the type matrix, and optionally sampling.

The pipeline groups assembled flows by `Source::name()` and runs them through `futures::join_all` — one async worker per source, sequential within. The CLI prints `running validation in N workers` at start. Output ordering is deterministic: results are sorted back into config order.

The flow-level `[flow.<name>.validation]` block exposes four toggles: `access`, `fields`, `inserts` (default `true`) and `sampling` (per-backend default — Mongo enabled at size 100, SQL disabled). Sampling-validation pulls rows via `Source::sample` (cursor-driven by default; CDC sources override with `$sample` since their `read_batch` would block) and runs the compiled Transform against them.

## Three-layer pipeline (Source → Transform → Sink)

Data moves through three explicit layers with disjoint responsibilities:

1. **Source** emits `Batch { rows: Vec<Row> }`. Each `Row` carries `values: Vec<Value>`, an optional `body: Option<Value>` and `op: RowOp`. Sources populate `body` only when `ReadSpec.needs_body == true`: relational sources fill `Value::Json(build_body_json(...))` (see `air_elt_core::transform::build_body_json`); Mongo wraps the document as `Value::Custom(BsonObjectValue)`. There is no separate `RawRow`/`RawBatch` — the same `Row`/`Batch` types travel through Transform and onto the sink (post-Transform rows carry `body = None`).
2. **Transform** is a pure interpreter at `crates/core/src/transform/`. The IR is closed: `TransformOp::{ Take { source_index }, Body, Convert { input, plan }, Switch { input, table } }`. `Transform::apply(batch) -> Batch` runs the program. The compile step caches an identity short-circuit when every column is `Take{i}` for `i in 0..len` — and when no row carries a body, `apply` returns the input batch unchanged. An "absorb-when-last" optimisation moves the value from the last reference to a given `Take{i}` slot or the `Body` payload; earlier references clone. `Switch` evaluates its `input` op, hashes the result through `SwitchKey::from_value`, and looks up the matched value (or `table.default`) in an `AHashMap`.
3. **Sink** consumes the resulting `Batch { rows: Vec<Row { values, body: None, op }> }` via `write_batch`. Sinks no longer perform per-cell conversion or body packing — Transform produced the final shape.

Validation-time hooks: `Source::body_data_type() -> DataType` (default `Json`; Mongo overrides to `DataType::Custom(BsonObjectType)`), and `DynType::is_object() -> bool` (default `false`; `BsonObjectType` overrides to `true`). The Transform compiler uses these to type-check `Body` sources and to permit the schemaless raw-passthrough fast path.

## Type model — canonical pivot, N+N matrix

Each source maps `native → DataType` on read, each sink maps `DataType → native` on write. This gives N+N mappings instead of N×N. The internal type set covers integers (signed + unsigned for MySQL/MariaDB UNSIGNED columns), floats, `BigInt { width }` and `Decimal { precision, scale }` for SQL `numeric`, sized `Text`/`Bytes`, `Date`, `Timestamp` (UTC), `Uuid`, `Ipv4` / `Ipv6` (host addresses; PG `inet` with subnet masks lives in `PgInetType` custom), `Json`, `Xml`. Nullability is a property of `Field`, not of `DataType` — `Value::Null` carries "no data".

Compatibility is checked at validation time by `core::types::matrix::is_compatible` (lossless) or `is_compatible_with_truncate` (when a mapping has `truncate=true`). Reverse paths (`BigInt/Decimal → Int*`, `Float ↔ BigInt/Decimal`) are deliberately rejected without `truncate`. Runtime per-cell conversion happens inside the Transform layer via `TransformOp::Convert` carrying a `ColumnConversionPlan`; identity columns lower to a bare `Take` (no convert).

Schemaless sinks (Mongo) opt out of the matrix narrowing check — `Sink::schemaless() == true` makes the pipeline derive the sink schema from the source's declared types.

For the full `DataType`/`Value` enumeration and conversion rules, read the source — `core::types`.

## Conflict resolution

Optional `[flow.<name>.conflict]` block with `key = […]` and `strategy = "ignore"|"overwrite"`. Without it, sinks do plain `INSERT` / `insertMany`. With it: pg uses `ON CONFLICT … DO NOTHING/UPDATE`, mysql uses `INSERT IGNORE` / `ON DUPLICATE KEY UPDATE … = VALUES(…)` (MariaDB-compatible legacy form), mongo upserts via parallel `replaceOne(upsert=true)` (single-key `["_id"]` takes a fast path).

## Config (TOML or YAML)

```toml
[config]
include = ["flows"]

[secrets]
DATABASE_PASSWORD = "$DB_PASS"

[[sources]]
name = "pg_src"
type = "postgres"
config = { url = "postgres://…" }

[[sinks]]
name = "pg_sink"
type = "postgres"
config = { url = "postgres://…" }

[[storages]]
name = "pg_state"
type = "postgres"
config = { url = "postgres://…" }

[flow.users]
source  = "pg_src"
sink    = "pg_sink"
storage = "pg_state"
from    = "public.users"
to      = "analytics.users"
[flow.users.mapping]
id = "id"
name = "name"
cursor  = { fields = ["id"], order = "asc", interval = "1s" }
batch-limit = 1024
```

Flow names must be unique across the root file and every `include`'d file. `mapping` is a TOML table keyed by sink column name; long-form entries accept only `from`, `truncate`, `default`, `switch` (`deny_unknown_fields`). Includes are unbounded in number, but each file is capped at 16 MiB.

For the full reference (every section, every field, every default, every validation rule) read the `config-format` skill.

## Fault tolerance

- Flows retry independently with exponential backoff (1s → 4× → 1h cap). One flow's failure does not affect others.
- `--once` mode propagates errors immediately (used by e2e tests).
- `save_cursor` failure aborts the iteration (not the process) to prevent duplicate writes.

## NULL-cursor algebra

Cursor columns may be nullable. NULL is treated as the minimum element: `NULL < any_non_null`, `NULL == NULL`. `ORDER BY` uses `ASC NULLS FIRST` / `DESC NULLS LAST` to match. Non-null cursors use plain `(c1,c2) > ($1,$2)`; if any cursor value is NULL, the source rewrites to a null-aware lexicographic predicate. ASC + all-NULL cursor reads all non-null rows; DESC + all-NULL cursor returns FALSE. Direction is per-column.

## CLI

- `air-elt validate --config <path>` — full validation pipeline (real access probes).
- `air-elt migrate --config <path>` — runs `Storage::migrate` for every declared storage.
- `air-elt run --config <path>` — daemon. `--once` drains a single tick and exits.
- `air-elt` (no subcommand) — shorthand for `run --config ./config.toml`.

## Out of MVP

Vault secret retrieval (only `$ENV_VAR` / literals work), privilege-excess check, OTel metrics, connectors beyond postgres + mysql + mongodb + cockroachdb.

## Testing

Inverted pyramid: heavy e2e tests against real databases, focused unit tests for pure logic. No database mocks. See `project-conventions::Testing` for handles and rules.

## Manual smoke test

A Python+uv orchestrator at `manual-tests/mongo-to-pg-10k-rps/` (not bash, cross-platform) runs a continuous mongo→pg pipeline for at least 5 minutes and captures replication lag plus daemon CPU/RSS. Containers persist after `run.py` exits — invoke `cleanup.py` explicitly to tear them down. See the folder's `README.md` for usage.
