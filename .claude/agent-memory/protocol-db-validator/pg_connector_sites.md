---
name: PostgreSQL connector call-site map
description: Where each integration concern lives across the three pg crates — useful for triaging future findings quickly
type: reference
---

## Identifier escaping
- `crates/commons/lib/src/sql/pg/identifier.rs` — single source of truth:
  `quote_ident`, `quote_qualified`, `quote_columns`, `split_qualified`.
  Policy: `[A-Za-z0-9_$]` per segment (rejects dots-inside-segment, spaces,
  hyphens, UTF-8 by design). `IdentifierError` converts to `RuntimeError`
  via `impl From` in the same module.

## SQL composition
- `crates/sources/postgres/src/sql_statements.rs` — PING,
  HAS_TABLE_SELECT, INFORMATION_SCHEMA, probe_select, build_read_batch
  (plain tuple-compare fast path + null-aware lex-compare with per-column
  direction).
- `crates/sinks/postgres/src/sql_statements.rs` — PING, HAS_TABLE_INSERT,
  INFORMATION_SCHEMA, probe_insert_where_false (wrapped in BEGIN/ROLLBACK
  by caller), insert_statement.
- `crates/storages/postgres/src/sql_statements.rs` — PING, TABLE_EXISTS,
  HAS_CREATE_PRIVILEGE, PROBE_INSERT_WHERE_FALSE, SELECT_CURSOR,
  UPSERT_CURSOR (JSONB ON CONFLICT DO UPDATE with `updated_at = now()`).
  No longer runs `CREATE SCHEMA` / `SET search_path` itself — schema is
  selected by the operator via `options=-c search_path=...` on the URL.

## Type mapping (N+N matrix)
- Canonical shared: `crates/commons/lib/src/sql/pg/pg_type.rs` — `PgType`
  enum + `parse` (accepts `udt_name` or `data_type` strings) +
  `to_internal`. `timestamp without time zone` is deliberately unsupported.
- Source-side thin re-export: `crates/sources/postgres/src/model/pg_type.rs`
  (`pub use …::{PgType, parse_or_err, to_internal}`).
- Sink-side: `crates/sinks/postgres/src/model/pg_type.rs` adds
  `from_internal(DataType) -> Result<PgType, TypeError>`. Timestamp maps
  to `TimestampTz` because canonical time is UTC.

## Value codec
- Decode: `crates/sources/postgres/src/model/codec.rs::decode_column` —
  typed `try_get::<Option<T>>` per `DataType`, NULL folds to `Value::Null`.
- Bind (cursor): `crates/sources/postgres/src/model/codec.rs::bind_cursor_value`
  — NULL path still binds `Option::<i64>::None` unconditionally (callers
  in `pg_source.rs` route through `bind_cursor_value (handles NULL via null_bind)` which dispatches to
  `null_bind::bind_typed_null` when the value is Null, so the wrong-OID
  branch in bind_cursor_value is effectively unreachable).
- Bind (sink bulk): `crates/sinks/postgres/src/pg_sink.rs::write_batch` —
  `QueryBuilder::push_values` with an inline typed-null match per DataType.
  Duplicates `null_bind::push_typed_null`; could be replaced.

## Typed NULL helpers
- `crates/commons/lib/src/sql/pg/null_bind.rs` — `bind_typed_null` (for
  `Query`) and `push_typed_null` (for `Separated` in `QueryBuilder`).
  13 arms covering every `DataType` variant.

## Connection
- Single entry point: `crates/commons/lib/src/sql/pg/pool.rs::connect`.
  Hard-coded `max_connections(5)`. Acquire/idle/max_lifetime come from
  `PoolTimeouts`. `after_connect` runs `SET TIME ZONE 'UTC'` and
  `SET statement_timeout = <ms>` on every new physical connection.
  Whole `connect_with` wrapped in `tokio::time::timeout(connect)`.
- All three connector crates (`PgSource`/`PgSink`/`PgStorage`) call
  this helper and pass `PoolTimeouts::from_options(...)` from their
  config.

## Migrations
- Files at `migrations/storage-postgres/0001_init.sql` (workspace-root dir).
- Invoked from `crates/storages/postgres/src/pg_storage.rs::migrate` with
  relative path `../../../migrations/storage-postgres` (resolved from
  the crate's `Cargo.toml`).
- `0001_init.sql`: `CREATE TABLE IF NOT EXISTS air_elt_cursors (flow TEXT
  PRIMARY KEY, state JSONB NOT NULL, updated_at TIMESTAMPTZ NOT NULL
  DEFAULT now())`. Unqualified — lands in whatever search_path the URL
  dictates.

## Test fixtures
- `crates/commons/testing/src/pg.rs` — `pg_pool()` returns `PgTestHandle`
  with `url_with_search_path()` that embeds `options=-c search_path=<schema>`.
  Podman-on-macOS auto-detect at `$TMPDIR/podman/*-api.sock`.
  Per-test sandbox: when `AIR_ELT_TEST_PG_URL` is set, creates a unique
  `test_<epoch>_<rand>` schema and drops it on Drop.
