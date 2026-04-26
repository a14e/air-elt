---
name: air-elt-overview
description: Architecture and product overview of the Air Elt ELT service — features, tech stack, workspace layout, and config format. Load this before making non-trivial changes so you understand the product shape, crate boundaries, and the GitOps/declarative flow model.
user-invocable: false
---

# Air Elt — project overview

Air Elt is a Rust service for moving data between systems with **minimal transformation** (ELT, not ETL). Data is carried through as-is; only essential coercion happens at the edges.

## Key features

1. **GitOps flows.** Each flow is a declarative TOML file describing source, sink, storage, column mapping, and cursor.
2. **Strict validation on startup.**
   1. Config schema / structural checks.
   2. Access checks — real connect + minimal probe (`SELECT 1`, `INSERT … SELECT … WHERE false`, etc.) against source/sink/storage.
   3. Target tables exist and current user has required privileges.
   4. *(Optional)* Excess-privilege check.
   5. Field/type compatibility between source and sink via the canonical-type matrix.
   6. *(Optional)* Sample-value conversion check.
3. **Micro-batch + drain.** Flows run sub-second micro-batches with drain semantics (pull while full, sleep when empty). A classic nightly mode is also allowed via a long interval.
4. **Monitoring is first-class.** All processes emit structured logs via `tracing`.
5. **Minimal resources.** Run centralised, or place instances next to each data plane (nginx-style).
6. **Static SQL.** All SQL is composed during config-init, not per-row.

## Tech stack

Rust 1.90 stable (pinned via `rust-toolchain.toml`), tokio, async-trait, sqlx (postgres / mysql / migrate / chrono / uuid / json), tracing, clap, mimalloc, thiserror.

## Workspace layout

```
air-elt/
├── rust-toolchain.toml
├── Cargo.toml                       # [workspace] + [workspace.package] + pinned [workspace.dependencies]
├── crates/
│   ├── app/                         # bin air-elt (CLI, mimalloc, tracing init, registry wiring)
│   ├── core/                        # traits, types, config, validation, flow runner
│   ├── commons/lib/                 # tracing-init, identifier, pool_timeouts (db-agnostic — no project deps)
│   ├── commons/pg/                  # pg quote/pool/schema/pg_type/null_bind
│   ├── commons/mysql/               # mysql quote/pool/schema/mysql_type/null_bind
│   ├── commons/testing/             # PgTestHandle / MySqlTestHandle + shared backend probe
│   ├── sources/{postgres,mysql}/    # connectors per backend
│   ├── sinks/{postgres,mysql}/
│   └── storages/{postgres,mysql}/   # storage + migrations dir per backend
├── migrations/storage-{postgres,mysql}/  # raw SQL for sqlx::migrate!
└── examples/{pg-to-pg,mysql-to-mysql}/   # usage examples
```

**Cross-dependencies between `sources/*`, `sinks/*`, `storages/*` are forbidden.** Each depends only on `core` (and optionally `commons`). Connectors are wired into the app via `core::registry::Registry`.

The `mysql` connector is exercised against both vanilla MySQL **and MariaDB** (10.7+ for native UUID, version-aware UPSERT for `VALUES()` legacy form). MariaDB is a *test target*, not a separate registered backend — there is no `type = "mariadb"` in config.

## Type model (canonical pivot, N+N matrix)

Internal canonical `DataType`s: `Bool, Int16, Int32, Int64, UInt8, UInt16, UInt32, UInt64, Float32, Float64, BigInt { width: Option<u32> }, Decimal { precision: Option<u32>, scale: Option<u32> }, Text { size: Option<u32> }, Bytes { size: Option<u32> }, Date, Timestamp (UTC), Uuid, Json`. Unsigned variants exist solely to carry MySQL/MariaDB `UNSIGNED` integer columns lossless — Postgres has no native unsigned ints, so PG schemas never produce `UInt*`. Nullability is a property of `Field`, not a type — `Value::Null` represents "no data". `Text`/`Bytes` carry the column's declared length (`varchar(36)`, `binary(16)`); `None` means unbounded (`text`, `blob`, etc.). `BigInt` is `numeric(p, 0)` (arbitrary-precision integer, backed by `num_bigint::BigInt` to skip BigDecimal arithmetic on plain integer pipelines); `Decimal` is `numeric(p, s>0)` (backed by `bigdecimal::BigDecimal`). PG `numeric` without modifier surfaces as `Decimal { precision: None, scale: None }`.

- Each source maps `native → DataType`, each sink maps `DataType → native`. This gives N+N mappings instead of N×N.
- **Cross-type conversion happens in `core::types::convert`.** The runner dispatches per cell only when `source_dt != sink_dt` (identity columns are skipped). Supported pairs: `Uuid ↔ Text` (size ≥ 36, accepts canonical / hex-no-dash / `{...}` formats), `Uuid ↔ Bytes` (size ≥ 16), `Int* ↔ Bool` (`0 ↔ false`, non-zero → `true`), plus numeric widening that leaves the value unchanged. `Int* → BigInt`, `Int*/BigInt → Decimal`, and BigInt/Decimal widening (target unbounded or wider precision/scale) are also supported. **Reverse paths (`BigInt → Int*`, `Decimal → BigInt/Int*`, `Float ↔ BigInt/Decimal`) are deliberately rejected — every one is potentially lossy.**
- **Compatibility is checked at validation time** by `types::matrix::is_compatible`. Width-narrowing and unbounded→bounded are rejected. Validation populates `FlowState::conversions` so the runner has a per-column plan ready.
- Config format details are in the `config-format` skill.

## Config (TOML only in MVP)

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
from    = "public.users"          # source table (dot-qualified allowed)
to      = "analytics.users"       # sink table
mapping = [{ from = "id", to = "id" }, { from = "name", to = "name" }]
cursor  = { fields = ["id"], order = "asc", interval = "1s" }
batch-limit = 1024
```

- Flow names must be unique across the root file and every `include`'d file.
- `mapping` accepts only `from`, `to`, `truncate`, `default` — `deny_unknown_fields` rejects every other key at parse time. The previously-reserved `transform` / `timezone` / `data-type` placeholders are gone, in line with the project rule "no future-proofing config fields" (see `rust-guidelines`).
- Architecturally we allow an unbounded number of `include` files — only the per-file 16 MiB cap applies.

## Operating philosophy

Validation phase prioritises **correctness**: every access probe, type check, and config constraint must pass before any data moves. Runtime phase prioritises **fault tolerance**: individual flow failures are logged and retried, never crashing the whole process.

## Fault tolerance

- Flows retry independently with exponential backoff (1s → 4x → 1h cap). One flow's failure does not affect others.
- `--once` mode propagates errors immediately.
- `save_cursor` failure aborts the iteration (not the process) to prevent duplicate writes.

## NULL-cursor algebra

Cursor columns may be nullable. NULL is treated as the minimum element: `NULL < any_non_null` and `NULL == NULL`. ORDER BY uses `ASC NULLS FIRST` / `DESC NULLS LAST` to match. Non-null cursors use plain `(c1,c2) > ($1,$2)`; if any cursor value is NULL, SQL rewrites to a null-aware lexicographic predicate. ASC + all-NULL cursor reads all non-null rows (NULL is minimum); DESC + all-NULL cursor returns `FALSE` (nothing below minimum). Direction is per-column.

## Explicit out-of-MVP list

- Vault secret retrieval (only `$ENV_VAR` / literals work).
- Privilege-excess check and sample-conversion check.
- Prometheus/OTel metrics.
- Connectors beyond postgres + mysql.
- YAML config.

## CLI

- `air-elt validate --config <path>` — full validation pipeline (runs real access probes).
- `air-elt migrate --config <path>` — runs `Storage::migrate` for every declared storage.
- `air-elt run --config <path>` — daemon (micro-batch + drain) with graceful shutdown on SIGTERM/Ctrl-C. Use `--once` to drain a single tick and exit (used by e2e tests).
- `air-elt` (no subcommand) — shorthand for `run --config ./config.toml`.

## Testing

Inverted pyramid: heavy e2e tests against real Postgres, focused unit tests for pure logic. No database mocks.