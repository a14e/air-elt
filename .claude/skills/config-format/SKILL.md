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
| `type` | string | yes | Connector kind (`"postgres"`, `"mysql"`, or `"mongodb"`) |
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

### MongoDB connector config (`config = { ... }`)

The same field set covers `[[sources]]`, `[[sinks]]`, and `[[storages]]` of `type = "mongodb"`.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | Connection URL (`mongodb://[user:pass@]host[:port][/db][?opts]`) |
| `database` | string | from URL path | Override the database name. Required if the URL has no path component. |
| `connect-timeout` | Duration | `"5s"` | TCP connect timeout |
| `acquire-timeout` | Duration | `"10s"` | Server-selection timeout |
| `idle-timeout` | Duration | `"5m"` | Idle connection lifetime |
| `max-connections` | u32 | 5 | Driver pool size cap (≤100) |
| `min-connections` | u32 | 0 | Minimum pool size |
| `schema-sample-size` | usize | 100 | (source only) Documents pulled by `describe_schema` for type inference |
| `operation-timeout` | Duration | `"30s"` | (source only) Per-op `maxTimeMS` applied to driver calls that support it (Find / Aggregate / FindOne). Bounds server-side work after the runner detaches a spawned future on shutdown / timeout. |
| `collection` | string | `"air_elt_cursors"` | (storage only) Collection that holds cursor state |

**Mongo-specific notes:**
- Flow `from` / `to` are bare collection names — no database prefix; database is configured on the connector itself.
- Mapping `from` / `to` accept dot notation for nested fields (`"addr.city"`). Dot notation is forbidden for SQL connectors.
- Cursor field set must be a single field in MVP (multi-key Mongo cursors are not yet supported).
- Mongo collections are schemaless. The sink takes any BSON shape; validation is driven by the source's *sampled* schema (see `[flow.<name>.validation]` below).

### `mongo-cdc` source config (`[[sources]] type = "mongo-cdc"`)

CDC source driven by `collection.watch()` change streams. Requires a **replica-set** Mongo deployment — change streams cannot run on standalone mongod.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | Connection URL (must point at a replica set, e.g. `mongodb://host:27017/?replicaSet=rs0`) |
| `database` | string | from URL path | Override the database name |
| `connect-timeout` / `acquire-timeout` / `idle-timeout` / `max-connections` / `min-connections` | — | — | Same shape as the `mongodb` connector |
| `operation-timeout` | Duration | `"30s"` | Per-op `maxTimeMS` for find / aggregate calls (sampling, lookup find) |
| `max-await-time` | Duration | `"1s"` | Long-poll cap on a single `change_stream.next()` await. Tune up for fewer wake-ups, down for lower per-tick latency. |
| `schema-sample-size` | usize | 100 | Documents pulled by `describe_schema`. Capped at 10 000. |

**Per-flow options** — the `mongo-cdc` source requires the developed `source = { name = "...", mode = "..." }` form on each referencing flow:

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `mode` | `"post-image"` / `"lookup-on-update"` | yes | How update events get a post-image. **PostImage** uses `fullDocument: "required"` on the watch options — needs `changeStreamPreAndPostImages` enabled on the collection (Mongo 6+). **LookupOnUpdate** opens the stream without `fullDocument` and issues one `find({_id: {$in: ids}})` per batch to attach the current state. |

```toml
[flow.users]
source = { name = "mongo_cdc", mode = "lookup-on-update" }
sink = "pg_sink"
storage = "pg_state"
from = "users"
to = "public.users"
mapping = [
    { from = "_id", to = "id" },
    { from = "name", to = "name" },
]
batch-limit = 500

# Required: cdc emits Upsert/Delete; the sink needs a key.
[flow.users.conflict]
key = ["id"]
strategy = "overwrite"
```

**Constraints:**
- `cursor.fields` is **forbidden** — pagination is the resume token, persisted via `Storage::save_resume_token` in a dedicated `air_elt_resume_tokens` table / collection (separate from `air_elt_cursors`).
- `[flow.<name>.conflict]` is **mandatory** — change events emit `Upsert` and `Delete`, both of which need a key.
- Drop / rename / dropDatabase / invalidate events fail the iteration; the runner retries with the saved resume token. If the token has aged out of the oplog the operator must intervene.
- **Bootstrap recipe**: pair `[[sources]] type = "mongo-cdc"` with a parallel `[[sources]] type = "mongodb"` snapshot flow on the same collection. After the snapshot catches up, disable it; the cdc flow keeps the table fresh — including DELETEs the snapshot source cannot observe.

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
| `validation` | table | `{}` | Optional per-flow validation knobs (see below) |
| `conflict` | table | absent | Optional upsert directive (see below). Without this block sinks do plain `INSERT` / `insertMany`. |

### `validation`

Optional sub-block; controls validation steps that go beyond schema introspection.

```toml
[flow.<name>]
validation = { sampling = true }
# or, table form:
[flow.<name>.validation.sampling]
enabled = true
size = 100
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `access` | bool | `true` | Run source / storage `validate_access` probes (ping, schema visibility). |
| `fields` | bool | `true` | Gates schema introspection (`describe_schema` on source + sink), `check_cursor`, and `check_mapping`. With `fields = false` no introspection runs; conversions become identity passthrough (`Json → Json`), `truncate` becomes a no-op, and `default` is rejected (parsing the literal needs the real sink type). Use this to bring an empty Mongo collection online before the first writer exists. For Mongo with `fields = true` the check is honoured but partial — the inferred schema is a sample, not authoritative. |
| `inserts` | bool | `true` | Run the sink write probe (insert + delete sentinel). |
| `sampling` | bool / table | per-backend default | Enable sampling-validation. `true` → enabled with size 100. `false` → disabled. Table form `{ enabled, size }` overrides the default size. |

**Backend defaults**: when `validation.sampling` is omitted, the source factory chooses — `mongodb` defaults **on** (size 100), `postgres` and `mysql` default **off**. Sampling pulls `size` rows from the source via the cursor query (`Source::sample`) and, for backends that support it (Mongo `$sample`), an extra random slice via `Source::sample_fresh`. Both row sets are run through every `ConversionPlan::convert`, surfacing data that violates the declared types (overflow integers, malformed UUIDs, etc.). The recommendation is to enable it on SQL flows whose data shape you don't fully trust — the cost is one extra round-trip per validate run.

### `conflict`

Optional upsert directive. Without this block sinks do plain `INSERT` / `insertMany`. With it, the sink upserts on `key` using the chosen `strategy`.

```toml
[flow.<name>.conflict]
key = ["id"]            # one or more sink columns / dot-paths
strategy = "overwrite"  # "ignore" | "overwrite"
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `key` | `[string]` | required | Sink columns / dot-paths forming the conflict key. Must be a subset of `mapping.to`. |
| `strategy` | `"ignore"` / `"overwrite"` | required | `ignore` drops the new row on key collision; `overwrite` replaces the existing row. |

**Per-backend translation**:
- **Postgres** — `ON CONFLICT (key) DO NOTHING` for `ignore`, `ON CONFLICT (key) DO UPDATE SET col = EXCLUDED.col, …` for `overwrite`. The `key` columns must form a unique index in the sink table; otherwise PG rejects the statement.
- **MySQL / MariaDB** — `INSERT IGNORE` for `ignore` (drops any unique-violation, not just the one on `key`), `ON DUPLICATE KEY UPDATE col = VALUES(col), …` (legacy form, MariaDB-compatible) for `overwrite`. The `key` columns must form a UNIQUE/PRIMARY KEY.
- **MongoDB** — `insertMany(ordered=false)` swallowing E11000 duplicate-key errors for `ignore`. For `overwrite` the path is server-version dependent: on **server ≥ 8.0** the sink uses `Client::bulk_write` (one round-trip per batch); on **older servers** it falls back to per-row `replaceOne(filter = key, upsert = true)` fired with bounded concurrency. The version is detected once at `connect()` via `db.runCommand({ buildInfo: 1 })`. Single-key `["_id"]` takes a fast path that skips the FieldPath round-trip on both branches.

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
- `conflict.key` ⊆ mapping `to` columns (when `[flow.<name>.conflict]` is set)
- File size ≤ 16 MiB
- No absolute include paths
- Symlink loops detected via canonical path dedup