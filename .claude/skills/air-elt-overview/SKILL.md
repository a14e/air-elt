---
name: air-elt-overview
description: Architecture and product overview of the Air Elt ELT service — features, tech stack, workspace layout, and config format. Load this before making non-trivial changes so you understand the product shape, crate boundaries, and the GitOps/declarative flow model.
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
4. **Monitoring is first-class.** All processes emit structured logs and metrics via `tracing`.
5. **Minimal resources.** Run centralised, or place instances next to each data plane (nginx-style).
6. **Static SQL.** All SQL is composed during config-init, not per-row.

## Tech stack

Rust 1.90 stable (pinned via `rust-toolchain.toml`), tokio, async-trait, sqlx (postgres / migrate / chrono / uuid / json), tracing, clap, mimalloc, thiserror. `unsafe` is forbidden by `AGENTS.md`.

## Workspace layout

```
air-elt/
├── rust-toolchain.toml
├── Cargo.toml                       # [workspace] + [workspace.package] + pinned [workspace.dependencies]
├── crates/
│   ├── app/                         # bin air-elt (CLI, mimalloc, tracing init, registry wiring)
│   ├── core/                        # traits, types, config, validation, flow runner
│   ├── commons/                     # tracing-init, secret resolve, sql::pg quoting, testing helpers
│   ├── sources/postgres/            # PgSource
│   ├── sinks/postgres/              # PgSink
│   └── storages/postgres/           # PgStorage + migrations
├── migrations/storage-postgres/     # raw SQL for sqlx::migrate!
└── examples/pg-to-pg/               # usage example
```

**Cross-dependencies between `sources/*`, `sinks/*`, `storages/*` are forbidden.** Each depends only on `core` (and optionally `commons`). Connectors are wired into the app via the `core::registry::Registry`.

## Type model (canonical pivot, N+N matrix)

Internal canonical `DataType`s: `Null, Bool, Int16, Int32, Int64, Float32, Float64, Text, Bytes, Date, Timestamp (UTC), Uuid, Json`.

- Each source defines `native_type → DataType` (e.g. `sources/postgres/src/model/pg_type.rs::to_internal`).
- Each sink defines `DataType → native_type` (same shape, opposite direction).
- **There is no canonical↔canonical value conversion.** `core::types::matrix::is_compatible` is a predicate used at validation time only: identity + safe widening + null-assignability.
- Narrowing / bool↔int / text↔scalar auto-coercion are rejected by validation. Users either fix schemas or add explicit transforms later (out of MVP).

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
batch_limit = 1024
```

- Flow names must be unique across the root file and every `include`'d file.
- `mapping` transform/timezone fields parse but fail with `UnsupportedInMvp` until transforms land.

## Explicit out-of-MVP list

- Vault secret retrieval (only `$ENV_VAR` / literals work).
- Value transforms (`transform = "seconds"`, `timezone`).
- Privilege-excess check and sample-conversion check.
- Prometheus/OTel metrics.
- Non-postgres connectors.
- YAML config.

## CLI

- `air-elt validate --config <path>` — full validation pipeline (runs real access probes).
- `air-elt migrate --config <path>` — runs `Storage::migrate` for every declared storage.
- `air-elt run --config <path>` — daemon (micro-batch + drain) with graceful shutdown on SIGTERM/Ctrl-C. Use `--once` to drain a single tick and exit (used by e2e tests).

## Testing

- Unit tests alongside the code (`#[cfg(test)] mod tests`).
- E2E tests in each crate's `tests/` folder via `air_elt_commons::testing::pg::pg_pool()`, which either connects to `AIR_ELT_TEST_PG_URL` (CI) or spins up an ephemeral postgres container (local via podman).
- Pyramid is inverted: few heavy e2e tests, few focused unit tests. No mocks for databases.
