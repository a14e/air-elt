---
name: config-format
description: Complete reference for the Air Elt TOML config format — all sections, fields, defaults, and validation rules. Load before editing config files, config structs, or the loader.
---

# Config format

Air Elt uses TOML. Multi-word keys use **kebab-case** (`batch-limit`, `operation-timeout-secs`).

## `[config]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `include` | `[string]` | `[]` | Relative paths to files or directories. Directories are scanned non-recursively for `*.toml`. Absolute paths rejected. |

## `[secrets]`

Flat `key = "value"` map. Used by `${VAR}` expansion (env → secrets → default → error). No recursion, no vault.

## `[[sources]]` / `[[sinks]]` / `[[storages]]`

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `name` | string | yes | Unique identifier |
| `type` | string | yes | Connector kind (`"postgres"` or `"mysql"`) |
| `config` | table | yes | Connector-specific config (see below) |

### Postgres / MySQL connector config (`config = { ... }`)

The same field set applies to both `"postgres"` and `"mysql"` types — only the URL scheme differs (`postgres://...` vs `mysql://...`). For MySQL, the database name is taken from the URL path (`mysql://user@host:3306/dbname`); table identifiers may also be schema-qualified (`appdb.users`).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | Connection URL |
| `connect-timeout` | Duration | `"5s"` | TCP connect timeout |
| `acquire-timeout` | Duration | `"10s"` | Pool acquire timeout |
| `idle-timeout` | Duration | `"5m"` | Idle connection lifetime |
| `max-lifetime` | Duration | `"30m"` | Max connection age |
| `statement-timeout` | Duration | `"30s"` | Postgres `SET statement_timeout` |
| `max-connections` | u32 | 5 | Pool size (capped at 100) |
| `min-connections` | u32 | 0 | Minimum idle connections |

## `[flow.<name>]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `source` | string | required | Source name |
| `sink` | string | required | Sink name |
| `storage` | string | required | Storage name |
| `from` | string | required | Source table (dot-qualified: `schema.table`) |
| `to` | string | required | Sink table |
| `mapping` | array | required | Column mapping rules |
| `cursor` | table | required | Cursor config |
| `batch-limit` | usize | 1024 | Max rows per batch. `batch-limit × mapping cols ≤ 60,000` |
| `query-timeout` | Duration | `"30s"` | Per-operation timeout for read/write/cursor calls |

### `mapping`

Simple form: `{ from = "col_a", to = "col_b" }`

Object form (**not supported in MVP** — parses but rejected with `UnsupportedInMvp`):
```toml
{ from = { name = "col_a", transform = "...", timezone = "...", data-type = "..." }, to = "col_b" }
```
Fields `transform`, `timezone`, `data-type` are reserved for future use. Any config using them will fail validation.

### `cursor`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `fields` | `[string]` | required | Cursor column(s), must be subset of mapping |
| `order` | `"asc"` / `"desc"` | `"asc"` | Cursor direction |
| `interval` | Duration | `"1s"` | Idle interval between drain ticks |

### Duration format

All Duration fields accept two formats, routed by prefix:

**ISO 8601** (`P`/`p` prefix): `PT1H30M`, `P1DT2H`, `P1W`, `PT1.5S`. Years/months rejected. Serialization always uses ISO 8601.

**Human-time** (everything else): `1h30m`, `500ms`, `1.5s`, `1 hour`, `3 days`. Units must be in decreasing order (w > d > h > m > s > ms). Bare number = seconds (`42` = 42s).

## Validation rules

- Flow names unique across root + includes
- `batch-limit ≥ 1`
- `batch-limit × mapping_cols ≤ 60,000`
- `cursor.interval > 0` (zero interval causes spin-loop)
- `query-timeout > 0` when specified
- Cursor fields ⊆ mapping `from` columns
- File size ≤ 16 MiB
- No absolute include paths
- Symlink loops detected via canonical path dedup