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

Two stages. **Assemble** (no I/O): looks up components by name, builds them through factories, derives `ReadSpec`/`WriteSpec`. **Validate** (I/O): runs access probes, schema introspection, the type matrix, and optionally sampling.

The pipeline groups assembled flows by `Source::name()` and runs them through `futures::join_all` — one async worker per source, sequential within. The CLI prints `running validation in N workers` at start. Output ordering is deterministic: results are sorted back into config order.

The flow-level `[flow.<name>.validation]` block exposes four toggles: `access`, `fields`, `inserts` (default `true`) and `sampling` (per-backend default — Mongo enabled at size 100, SQL disabled). Sampling-validation pulls rows via `Source::sample` (cursor-driven) and `Source::sample_fresh` (random, Mongo only) and runs every non-identity `ConversionPlan` against them.

## Type model — canonical pivot, N+N matrix

Each source maps `native → DataType` on read, each sink maps `DataType → native` on write. This gives N+N mappings instead of N×N. The internal type set covers integers (signed + unsigned for MySQL/MariaDB UNSIGNED columns), floats, `BigInt { width }` and `Decimal { precision, scale }` for SQL `numeric`, sized `Text`/`Bytes`, `Date`, `Timestamp` (UTC), `Uuid`, `Json`, `Xml`. Nullability is a property of `Field`, not of `DataType` — `Value::Null` carries "no data".

Compatibility is checked at validation time by `core::types::matrix::is_compatible` (lossless) or `is_compatible_with_truncate` (when a mapping has `truncate=true`). Reverse paths (`BigInt/Decimal → Int*`, `Float ↔ BigInt/Decimal`) are deliberately rejected without `truncate`. Runtime per-cell conversion happens in `core::types::convert`; the runner skips identity columns.

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
mapping = [{ from = "id", to = "id" }, { from = "name", to = "name" }]
cursor  = { fields = ["id"], order = "asc", interval = "1s" }
batch-limit = 1024
```

Flow names must be unique across the root file and every `include`'d file. `mapping` accepts only `from`, `to`, `truncate`, `default` (`deny_unknown_fields`). Includes are unbounded in number, but each file is capped at 16 MiB.

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

Vault secret retrieval (only `$ENV_VAR` / literals work), privilege-excess check, Prometheus/OTel metrics, connectors beyond postgres + mysql + mongodb + cockroachdb.

## Testing

Inverted pyramid: heavy e2e tests against real databases, focused unit tests for pure logic. No database mocks. See `project-conventions::Testing` for handles and rules.
