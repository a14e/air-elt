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

Single flat shape (object form with reserved future-fields was removed — see the no-future-proofing rule in `rust-guidelines`):

```toml
mapping = [
  { from = "col_a", to = "col_b" },
  { from = "long_text",  to = "summary",   truncate = true },
  { from = "blob_in",    to = "blob_out",  default = "hex:00" },
]
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `from` | string | required | Source column name |
| `to` | string | required | Sink column name |
| `truncate` | bool | `false` | Opt the column into narrowing conversions: text/bytes shrink (UTF-safe for text), integer/float saturate to target's max/min, decimal scale drop, json/xml → `text(n)` serialize. Forbidden combinations (`Json → Json`, `Xml → Xml`, UUID truncations, `Date → Timestamp`) remain rejected. |
| `default` | scalar / table | none | Fallback value substituted when the source value is `Null`. Permits mapping a nullable source into a `NOT NULL` sink. Validation rejects `default` if the source column is `NOT NULL` (the substitution would never fire). The literal is parsed against the resolved sink `DataType` (see grammar below). |

`#[serde(deny_unknown_fields)]` rejects any additional keys at parse time — this includes the previously-reserved `transform`, `timezone`, `data-type` placeholders, which are now removed.

#### `default` value grammar

| Sink type | Literal | Example |
|-----------|---------|---------|
| `Bytes` | `"hex:<even-hex>"`, `"base64:<b64>"`, `"utf8:<utf8>"`, `"bin:<bits>"` (length must be byte-aligned, no whitespace) | `default = "hex:deadbeef"` |
| `Text` | TOML string; UTF-char count ≤ declared `size` | `default = "n/a"` |
| `Bool` | TOML bool only | `default = false` |
| `Int*` / `UInt*` | TOML integer; range-checked | `default = 0` |
| `Float*` | TOML float / int | `default = 0.0` |
| `BigInt(width)` / `Decimal(p, s)` | TOML string for big numbers (recommended) or numeric literal | `default = "12.34"` |
| `Date` | ISO 8601 date string | `default = "1970-01-01"` |
| `Timestamp` | RFC 3339 string | `default = "1970-01-01T00:00:00Z"` |
| `Uuid` | canonical UUID string | `default = "00000000-0000-0000-0000-000000000000"` |
| `Json` | any TOML value | `default = { a = 1 }` |
| `Xml` | well-formed XML string (validated via `quick-xml`) | `default = "<root/>"` |

`Bytes` columns require one of the four prefixes; bare strings are rejected. Other types use the plain literal — there is no prefix grammar.

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