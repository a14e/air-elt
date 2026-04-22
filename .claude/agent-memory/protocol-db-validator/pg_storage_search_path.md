---
name: PgStorage search_path handling
description: PgStorage relies on operator-supplied `options=-c search_path=...` in the URL; no explicit SET is done. Migrations and runtime queries agree automatically.
type: project
---

Current behavior in `crates/storages/postgres/src/pg_storage.rs::migrate`:

1. Pool opened via `commons::sql::pg::pool::connect`. `after_connect` runs
   `SET TIME ZONE 'UTC'` + `SET statement_timeout` — no `SET search_path`.
2. `sqlx::migrate!` runs `0001_init.sql` which does
   `CREATE TABLE IF NOT EXISTS air_elt_cursors (...)` — unqualified.
3. Operators who need a non-default schema embed `?options=-c search_path=...`
   in `PgStorageConfig.url`. libpq applies these on every new connection,
   so *every* pool connection (including the one sqlx Migrator acquires and
   the ones runtime load/save use) lands on the same search_path.

The prior anti-pattern (SET search_path on the pool → lost when migrator
acquires a fresh connection) is **no longer present**. sqlx-postgres'
internal `_sqlx_migrations` table is also unqualified, so it lives in
the same search_path as `air_elt_cursors` — consistent.

**Why:** This is the cleanest way to avoid the "SET is per-connection"
trap without adding a bespoke schema field to the storage config.

**How to apply:** Any new connector that needs connection-scope configuration
must use `PgPoolOptions::after_connect` or libpq URL options. Do not call
`SET search_path` / `SET ROLE` against the pool directly — it only affects
one connection.

Reviewed: `crates/storages/postgres/src/pg_storage.rs`,
`crates/storages/postgres/src/config/model.rs`,
`migrations/storage-postgres/0001_init.sql`.
