---
name: config-format
description: Complete reference for the Air Elt TOML/YAML config format — all sections, fields, defaults, and validation rules. Load before editing config files, config structs, or the loader.
user-invocable: false
---

# Config format

Air Elt accepts both TOML (`.toml`) and YAML (`.yml`/`.yaml`). Format is detected per file by extension; mixing formats inside one include graph is allowed. The shape is identical — a TOML `[[sources]]`/`[flow.<name>]`/inline-table maps mechanically to a YAML list / nested map / nested mapping under the same keys. All examples below are TOML; translate to YAML by that mapping when needed. Multi-word keys use **kebab-case** (`batch-limit`, `operation-timeout-secs`) in both formats.

**Expression syntax in YAML vs TOML.** In TOML, expression strings require outer quotes: `default = "concat('a', 'b')"`. In YAML, values are already strings — write expressions without outer quotes: `default: concat('a', 'b')`. Same for `url: env('PG_URL')`, `default: if(true, 'yes', 'no')`, etc.

## Include & duplicate rules

- Each `[[sources]]`, `[[sinks]]`, `[[storages]]` entry and each `[flow.<name>]` must be defined in exactly one file across the root and all included files. Duplicate names across files are an error (`DuplicateName` / `DuplicateFlow`).
- Each `[secrets]` key must also be defined exactly once across the include graph. Duplicates are a `DuplicateSecret` error (was silently first-wins before).
- 16 MiB per-file size cap and absolute-path-include rejection apply to both formats.
- When invoked without `--config`, the CLI probes `./config.toml`, then `./config.yml`, then `./config.yaml`, and uses the first one found.
- A YAML file may contain multiple `---`-separated documents. Their `sources` / `sinks` / `storages` arrays concatenate, the `flow` map and `secrets` map merge, and `config.include` concatenates — exactly as if each document were a separate include. The "one definition per name, anywhere" rule still applies: declaring the same source/sink/storage/flow/secret name in two documents of one file is the same `DuplicateName` / `DuplicateFlow` / `DuplicateSecret` error a cross-file collision would raise. TOML has no equivalent — single document per `.toml` file.

## `[config]`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `include` | `[string]` | `[]` | Relative paths to files or directories. Directories are scanned non-recursively for `*.toml`, `*.yml`, and `*.yaml`. Absolute paths rejected. |

## `[secrets]`

Flat `key = "value"` map. Used by `${VAR}` expansion (env → secrets → default → error). No recursion, no vault.

## Expression support

Config string values support expressions in these fields:
- `default` in mapping entries — evaluated via `Evaluator::evaluate_expr_value()`
- Switch table values (RHS of switch pairs) — evaluated via `Evaluator::evaluate_expr_value()`
- `[secrets]` values
- Component config string values (URLs, connection strings) — via `ConfigExprPatcher` in `loader::load`

Detection rules:
- String starting with `name(...)` → evaluated as expression
- String containing `{expr}` → string interpolation
- `$$` in raw text escapes to literal `$`
- `{{` inside interpolation escapes to literal `{`
- Plain values (integers, booleans, non-expression strings) pass through unchanged

Examples:
- `default = "env('DB_HOST', 'localhost')"`
- `default = "concat(env('PREFIX'), '_suffix')"`
- `default = "if(isNull(env('OPT')), 'none', env('OPT'))"`
- `default = { "key" = "env('X')", "ts" = "now()" }`

### Component config expressions

String values inside source/sink/storage `config = { ... }` tables are evaluated as expressions before factory deserialization via `ConfigExprPatcher` (`crates/expr/runtime/src/patcher.rs`). For TOML configs, the patcher walks the raw TOML tree using trie-based pattern matching (patterns: `sources[*].config`, `sinks[*].config`, `storages[*].config`) before deserialization. For YAML configs, component config tables are patched after deserialization. Every string value detected as an expression or interpolation is evaluated; non-string values and plain string literals pass through unchanged. The result is coerced back to a TOML value. Resolution runs inside `loader::load` (`crates/core/src/config/loader.rs`) before `assemble`.

This means every string field in every connector config supports expressions and interpolations, including credentials and connection parameters:
- **Postgres / CockroachDB / MySQL**: `url`
- **MongoDB / Mongo-CDC**: `url`, `database`
- **ClickHouse**: `url`, `database`, `user`, `password`

```toml
# Postgres — url from environment
[[sinks]]
name = "pg"
type = "postgres"
config = { url = "env('PG_URL')", connect-timeout = "5s" }

# ClickHouse — credentials from environment
[[sinks]]
name = "ch"
type = "clickhouse"
config = { url = "env('CH_URL')", database = "analytics", user = "env('CH_USER')", password = "env('CH_PASSWORD')" }

# Interpolation works in any string field:
# url = "postgres://{env('PG_HOST')}:5432/db"
# password = "file('secrets/ch_password.txt')"
```

## `[[sources]]` / `[[sinks]]` / `[[storages]]`

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `name` | string | yes | Unique identifier |
| `type` | string | yes | Connector kind (`"postgres"`, `"cockroachdb"`, `"mysql"`, `"mongodb"`, `"mongo-cdc"` (source only), `"clickhouse"` (sink only), `"redis"` (sink only)) |
| `config` | table | yes | Connector-specific config (see below) |

### Postgres / CockroachDB / MySQL connector config (`config = { ... }`)

The same field set applies to `"postgres"`, `"cockroachdb"`, and `"mysql"` — only the URL scheme/port differs (`postgres://...:5432`, `postgres://root@...:26257/...?sslmode=disable` for Cockroach, `mysql://...:3306`). `cockroachdb` reuses the Postgres connector crates with `Dialect::Cockroach` selecting the divergent paths (automatic retry on `40001 RETRY_SERIALIZABLE`, upfront `XML`-type rejection, advisory-lock-free migrations). The standard `INSERT … ON CONFLICT` path is used for upserts on both engines. For MySQL, the database name is taken from the URL path (`mysql://user@host:3306/dbname`); table identifiers may also be schema-qualified (`appdb.users`).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | Connection URL |
| `connect-timeout` | Duration | `"5s"` | TCP connect timeout |
| `acquire-timeout` | Duration | `"10s"` | Pool acquire timeout |
| `idle-timeout` | Duration | `"5m"` | Idle connection lifetime |
| `max-lifetime` | Duration | `"30m"` | Max connection age |
| `statement-timeout` | Duration | `"30s"` | Postgres `SET statement_timeout` |
| `max-connections` | u32 | 5 | Pool size (capped at 100). Also the **validation AND runtime** concurrency cap for this component — `assemble` builds a `tokio::sync::Semaphore` of this many permits, and every flow referencing this component acquires one permit before running its access probes / sampling (validation) and across each tick's `read_batch → transform → write_batch → save_cursor` (runtime). The semaphore keeps simultaneous pool acquisitions below the operator-declared budget so the underlying `acquire-timeout` is only a safety backstop. |
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
| `max-connections` | u32 | 5 | Driver pool size cap (≤100). Doubles as the validation AND runtime concurrency cap for this component (see Postgres `max-connections` for details). |
| `min-connections` | u32 | 0 | Minimum pool size |
| `schema-sample-size` | usize | 100 | (source only) Documents pulled by `describe_schema` for type inference |
| `operation-timeout` | Duration | `"30s"` | (source only) Per-op `maxTimeMS` applied to driver calls that support it (Find / Aggregate / FindOne). Bounds server-side work after the runner detaches a spawned future on shutdown / timeout. |
| `collection` | string | `"air_elt_cursors"` | (storage only) Collection that holds cursor state |

**Mongo-specific notes:**
- Flow `from` / `to` are bare collection names — no database prefix; database is configured on the connector itself.
- Mapping `from` / `to` accept dot notation for nested fields (`"addr.city"`). Dot notation is forbidden for SQL connectors.
- Cursor field set must be a single field in MVP (multi-key Mongo cursors are not yet supported).
- Mongo collections are schemaless. The sink takes any BSON shape; validation is driven by the source's *sampled* schema (see `[flow.<name>.validation]` below).

### ClickHouse sink config (`[[sinks]] type = "clickhouse"`)

ClickHouse is **sink-only** today (no source, no storage). The sink declares `supports_deletes() = false`: the runner drops `RowOp::Delete` rows before `write_batch`, the validation pipeline skips `validate_delete_access`, and CDC sources may pair with it without a mandatory `[flow.<name>.conflict]` block (append-only ingest). The MergeTree family has no cheap `DELETE`/`UPDATE`; emulating deletes via `ALTER TABLE … DELETE` mutations is intentionally not supported.

INSERTs use the HTTP `RowBinary` format. Authentication is over standard CH `X-ClickHouse-User` / `X-ClickHouse-Key` headers.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | HTTP endpoint URL (e.g. `http://localhost:8123`). Use `https://` for TLS. No trailing slash. |
| `database` | string | required | Default database. Applied as `X-ClickHouse-Database`; flow `to` may still be `db.table` qualified to override per flow. |
| `user` | string | required | CH username. Required at deserialize time — omitting it surfaces `missing field 'user'` before any I/O. For the authless variant (CH with `<networks><ip>::/0</ip></networks>` open for the `default` user) write `user = "default"` and `password = ""`. |
| `password` | string | required | Password matching `user`. Required at deserialize time. Use `""` for the authless variant. Pair with `[secrets]` to avoid leaking it in the config file. |
| `connect-timeout` | Duration | `"5s"` | TCP connect timeout. |
| `idle-timeout` | Duration | `"5m"` | Idle HTTP connection lifetime. |
| `request-timeout` | Duration | `"30s"` | Whole-request cap (connect + send + server compute + body download). CH has no per-statement timeout exposed over HTTP. |
| `max-connections` | u32 | 5 | HTTP pool size cap. Doubles as the validation AND runtime concurrency cap for this component (see Postgres `max-connections` for details). |

**Unsupported pool fields.** Because the CH sink uses an HTTP client (reqwest) rather than a database connection pool, the fields `acquire-timeout`, `max-lifetime`, and `min-connections` — present in the Postgres / MySQL / MongoDB connectors — are **not supported**. Specifying any of them raises a `ConfigError::Invalid` at load time naming the offending field. This is intentional: silently ignoring a timeout the operator set would be misleading.

**Type mapping.** Native CH types are parsed from `system.columns.type`. `Nullable(T)` is stripped onto `Field.nullable`; `LowCardinality(T)` is stripped transparently. Canonical pivots: `String → Text`, `UInt*`/`Int8/16/32/64` → `UInt*`/`Int8/16/32/64`, `Float32/64 → Float32/64`, `Bool → Bool`, `Date`/`Date32 → Date`, `DateTime`/`DateTime64(N[, tz]) → Timestamp` (timezone qualifier parsed and discarded — stored UTC; TZ-aware paths land in a follow-up), `Decimal(P, S)` and `Decimal32/64/128/256(S) → Decimal{P, S}` (width selected automatically by precision — ≤9 → i32, ≤18 → i64, ≤38 → i128, ≤76 → i256 LE; value scaled by `10^S`), `UUID → Uuid`, `JSON`/`Object` → `Json`. `Int8` is a full canonical pivot — the RowBinary encoder writes it as 1 byte (two's-complement bit-cast via `i8 as u8`).

**Custom types (`DataType::Custom`)**:
- `clickhouse.ipv4` / `clickhouse.ipv6` — convert ↔ `Text` (canonical "x.x.x.x" / RFC 5952).
- `clickhouse.fixed_string` — convert ↔ `Bytes(N)`. Non-UTF8 binary, no `Text` path.
- `clickhouse.enum8` / `clickhouse.enum16` — convert ↔ `Text` (variant name).
- `clickhouse.int128` / `clickhouse.uint128` — `Int128` / `UInt128` columns; 16-byte LE. Convert ↔ `BigInt`.
- `clickhouse.int256` / `clickhouse.uint256` — `Int256` / `UInt256` columns; 32-byte LE two's-complement. Convert ↔ `BigInt`.
- `clickhouse.aggregate.<fn>` (`AggregateFunction` / `SimpleAggregateFunction` states for `quantilesTDigest`, `quantilesDDSketch`, `uniq*`, etc.) — opaque bytes, CH↔CH only.

**Structural composite types** (`Tuple`, `Array`, `Map`, `Nested`, `Point`, `Ring`, `Polygon`, `MultiPolygon`, …) are parsed as canonical `Json`. The CH server accepts JSON-encoded text for these via RowBinary; the round-trip is lossless.

### Redis / Valkey sink config (`[[sinks]] type = "redis"`)

Redis is **sink-only**. The per-mode columns (`key` / `value` / `ttl`) have **known canonical types**, so the validation matrix type-checks the mapped columns at config time (an `Int → key` or `Text → ttl` mapping fails at validate, not at runtime). What the matrix *can't* express — which columns each mode requires vs. allows — the sink enforces itself in `validate_access` / `build_context`. `[flow.<name>.conflict]` is **rejected** — redis writes are always last-write-wins (`SET`) or unconditional appends; there is nothing to arbitrate. Writes go over a standard `deadpool-redis` connection pool (one connection per checkout; a whole-batch pipeline rides one connection in a single round-trip); the URL and pool live on the connector, the write **mode** is per-flow.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | Connection URL. `redis://` (plaintext) or `rediss://` (TLS). |
| `pool` | table | `{}` | Connection-pool tunables (all optional). See below. |

`config = { url, pool = { ... } }` pool sub-fields (kebab-case):

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max-connections` | u32 | 10 | Pool size — number of connections, capped at 100. Also sizes the runtime concurrency semaphore for this sink (one permit per connection). |
| `connect-timeout` | Duration | `"5s"` | TCP connect timeout for dialing a new connection (deadpool `create`). |
| `acquire-timeout` | Duration | `"10s"` | Max wait for a free connection when the pool is saturated (deadpool `wait`). |
| `recycle-timeout` | Duration | `"5s"` | Bound on the health-check (`PING`) run when an idle connection is re-checked out (deadpool `recycle`). |

**Per-flow `mode`** — every referencing flow uses the developed sink form `sink = { name = "...", mode = "..." }` (mirror of the `mongo-cdc` source mode). Bare `sink = "redis_sink"` defaults to `kv`. The `mode` fixes the redis command and the required/optional mapped sink columns (resolved by **name** — `key`, `value`, `ttl`):

| `mode` | columns | command |
|--------|---------|---------|
| `kv` | `key` (Text, req), `value` (Json, req), `ttl` (Interval, **opt**) | `SET {to}{key} {json} [PX ttl]` |
| `kv-delete` | `key` (Text, req) | `DEL {to}{key}` (issued per row regardless of `RowOp`) |
| `list` | `value` (Json, req), `key` (Text, **opt**) | `RPUSH {to}{key?} {json}` (no `key` → list `{to}`) |
| `stream` | `key` (Text, req), `value` (Json, req) | `XADD {to}{key} * data {json}` |
| `pubsub` | `value` (Json, req), `key` (Text, **opt**) | `PUBLISH {to}{key?} {json}` (no `key` → channel `{to}`) |

Authoring: `value` is an object-literal compute (`{ "k" = `+"`col`"+` }` → `Value::Object` → JSON); `ttl` is a duration literal (`"10s"`, `"1h30m"` → `Value::Interval` → `PX` milliseconds). `key` must resolve to `Text` at write time (a null key errors for required-key modes; for optional-key modes it falls back to the bare `{to}`). The full redis key is the **plain concatenation `{to}{key}`** — the sink inserts no separator; put any `:` into `to` (`to = "users:"`) or the computed `key`.

```toml
[flow.users]
source = "pg_src"
sink = { name = "redis_sink", mode = "kv" }
storage = "pg_state"
from = "public.users"
to = "users:"                      # full key = users:{key}
[flow.users.mapping]
key = "uid"                        # TEXT cursor column → redis key suffix
[flow.users.compute-mapping]
value = '{ "name" = `name`, "age" = `age` }'   # object literal → JSON
ttl = "1h"                         # duration literal → Interval → PX
[flow.users.cursor]
fields = ["uid"]                   # cursor field must appear in mapping.from
order = "asc"
interval = "1s"
```

**Delivery semantics:** at-least-once **send to Redis** — not consumer delivery; Redis itself may evict/drop. `kv` / `kv-delete` are idempotent under a batch retry; `list` / `stream` / `pubsub` may **duplicate** (the runner re-delivers a failed batch). A whole batch is pipelined in one round-trip; a server error on any command fails the batch.

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
[flow.users.mapping]
id = "_id"
name = "name"
batch-limit = 500

# Required: cdc emits Upsert/Delete; the sink needs a key.
[flow.users.conflict]
key = ["id"]
strategy = "overwrite"
```

**Constraints:**
- `cursor.fields` is **forbidden** — pagination is the resume token, persisted via `Storage::save_resume_token` in a dedicated `air_elt_resume_tokens` table / collection (separate from `air_elt_cursors`).
- `[flow.<name>.conflict]` is **mandatory** — change events emit `Upsert` and `Delete`, both of which need a key. **Exception**: when the sink declares `supports_deletes() = false` (today: `clickhouse`), the conflict block becomes optional — the runner drops every `RowOp::Delete` pre-write, so the flow streams CDC events into the sink as append-only inserts.
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
| `mapping` | table | required | Column mapping rules, **keyed by sink column name**. See `mapping` section below. |
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

**Backend defaults**: when `validation.sampling` is omitted, the source factory chooses — `mongodb` defaults **on** (size 100), `postgres` and `mysql` default **off**. Sampling pulls `size` rows from the source via `Source::sample` (default impl drives the cursor query the runner uses; CDC sources override with `$sample` on the watched collection because their `read_batch` would block). Rows are run through the compiled Transform (each `ColumnConversionPlan` dispatches via `core::types::convert`), surfacing data that violates the declared types (overflow integers, malformed UUIDs, etc.). The recommendation is to enable it on SQL flows whose data shape you don't fully trust — the cost is one extra round-trip per validate run.

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

Mapping is an **inverted TOML table** keyed by sink column name. The right-hand side is either a bare string (interpreted as the source column `from`) or a long-form inline table carrying the full feature set (`truncate`, `default`, `switch`).

```toml
[flow.<name>.mapping]
# Identity / rename — bare string RHS is always `from`. Identity is the
# case `key == value`; rename is `key != value`. No separate forms.
field4 = "field4"
display = "user_name"

# Long-form table. The sink column name is the key — there is NO `to`
# field inside the inline table.
summary = { from = "long_text", truncate = true }
blob_out = { from = "blob_in",  default = 0 }

# Body-pack: route the row body into a single sink column. RHS `"*"`
# triggers it when the key is a regular column name.
body = "*"

# Wildcard expansion: the literal pair `"*" = "*"`.
"*" = "*"

# Switch: value-to-value lookup.
status_label = { from = "status", switch = { ACTIVE = "active", FINISHED = "finished" }, default = "unknown" }
```

**Long-form fields**

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `from` | string | required | Source column name |
| `truncate` | bool | `false` | Opt the column into narrowing conversions: text/bytes shrink (UTF-safe for text), integer/float saturate to target's max/min, decimal scale drop, decimal → float magnitude overflow saturates to `±INFINITY`, json/xml → `text(n)` serialize. `Decimal → Float64`/`Float32` is lossless without `truncate` when the declared precision fits the target's mantissa (≤ 15 / ≤ 7 respectively); wider or unbounded Decimal requires `truncate = true`. Forbidden combinations (`Json → Json`, `Xml → Xml`, UUID truncations, `Date → Timestamp`, `Float → Decimal/BigInt`) remain rejected. |
| `default` | scalar / table | none | Fallback value substituted when the source value is `Null` and when `switch` produces no match. Permits mapping a nullable source into a `NOT NULL` sink. On the Direct path validation rejects `default` if the source column is `NOT NULL`. The value is expression-evaluated via `Evaluator::evaluate_expr_value()` and checked against the resolved sink `DataType` (see `default` value evaluation below). |
| `switch` | inline table | none | Value-to-value lookup. Keys (inline-table keys — always strings in TOML) are parsed against the source column's `DataType`; values are parsed against the sink column's `DataType` (or contribute to union-collapse for schemaless sinks). Output: the matched value, or `default` on miss / NULL input, or `Value::Null` if no `default`. See **Switch** below. |
| `compute` | string | none | Expression **script** producing the column's value per row. Mutually exclusive with `from` and `switch`. Unlike `default` (a fixed compile-time value), a compute script runs **per row in the Transform** and may read source columns via `field("col")` / `` `col` `` (backtick) and `fields("*")`. `truncate` / `default` still apply (the `default` is the NULL-fallback of the computed value). See **Compute columns** below. |

`#[serde(deny_unknown_fields)]` rejects any additional keys at parse time on the long form — including a stray `to` field (the map key already carries it).

**Compute columns**

A `compute` mapping declares a **runtime script** — the only place expression scripts run per row (everything else, `default` / switch values, is evaluated once at assemble time). Two surfaces:

```toml
[flow.<name>.mapping]
# long form (carries truncate / default):
total = { compute = "`price` * `qty`", truncate = true }

[flow.<name>.compute-mapping]
# shorthand table: each value is a bare expression string.
# Backtick column refs read naturally here; truncate = false, default = null.
full_name = "concat(`first`, ' ', `last`)"
ingested_at = "now()"

# multiline if-expression script (TOML """ string; newlines separate statements):
tier = """
if (`spend` > 1000) {
  bonus = `spend` * 0.1
  concat('gold:', toString(bonus))
} else if (`spend` > 100) 'silver'
else 'basic'
"""
```

Inside a script, source columns are read with `field("col")` or the backtick literal `` `col` `` (prefer the backtick in YAML — no inner quoting). `fields("a,b")` builds an object of the named columns; `fields("*")` builds an object of the whole row. Every column a script reads is auto-added to the source read projection — you do **not** list them under `from`. `fields("*")` pulls in the entire source schema (for Mongo, the sampled schema, since compute requires `validation.fields = true`), so the packed object carries every column the schema knows.

Lowering (transparent, but explains the cost): a script that const-folds to a literal (e.g. `1 + 2`, `now()` is *not* const-folded — it is per-batch) becomes a plain constant column; a bare `field("x")` becomes an identity `Take` (a normal rename); anything else compiles to a per-row `Compute` op whose result is coerced to the sink column type. `now()` / `today()` are pinned to one timestamp **per batch** (SQL `NOW()` semantics) — every row in a write batch shares the same clock.

Requires `validation.fields = true` (the sink type must be known to type-check the script). The script's result type is checked against the sink column at validate time (`ComputeCompile` on mismatch).

**Short-form grammar** — string RHS:

| Pair | Meaning |
|------|---------|
| `key = "value"` (value ≠ `"*"`) | `from = "value", to = "key"`. Identity and rename collapse to this case. |
| `key = "*"` (key ≠ `"*"`) | Body-pack into sink column `key`. Lowers to `Body { to = key }`. |
| `"*" = "*"` | Wildcard expansion (see **Wildcard** below). |
| `"*" = "anything-else"` | Rejected — wildcard key only accepts wildcard value. |

Whitespace inside the RHS string is rejected. Multiple body-pack entries with distinct keys are allowed (post-expansion uniqueness still enforced). Combining `"*" = "*"` with any body-pack entry is rejected — they're mutually exclusive.

The legacy `mapping = [...]` array form (with `{ from, to, ... }` entries or shorthand strings like `"name"`, `"a:b"`, `"*:body"`) is **removed**. The deserialiser rejects it with a clear error pointing at the new shape.

**Wildcard `"*"`**

Resolution order: prefer the **sink** schema, fall back to the **source** schema, fall back to **raw passthrough**. Raw passthrough is admitted only when both `Source::schemaless()` and `Sink::schemaless()` are `true` AND `source.body_data_type().is_object()` is `true` (today: Mongo source + Mongo sink, since `BsonObjectType::is_object() = true`). The lowered mapping is empty `direct` plus a single body slot under the synthetic target `_root` (`ROOT_BODY_TARGET`), compiled to one `TransformOp::Body`. Sources whose body type is non-object surface `WildcardWithoutSchema`. Otherwise — both schemas absent on a non-schemaless pair — also fails with `WildcardWithoutSchema`.

Expansion ordering is locked: wildcard fills first in **schema declaration order**; explicit entries iterate in user-declaration order; an explicit whose `to` matches a wildcard slot **replaces in place**, otherwise it is **appended**. A `Body { to }` mapping always appends one synthetic sink column.

When wildcard expansion picks the sink schema and a sink column has no same-named source column, the sink column must be **nullable** — the runner injects `Value::Null` for that slot at runtime. A non-nullable column with no source pair fails with `WildcardMissingNonNullableSource` (recovery: declare the column long-form with `default = ...`).

The matrix is NOT relaxed under `*` — same-name pairs still go through the N+N type check; mismatches require an explicit long-form entry with `truncate` / `default`.

`cursor.fields` and `conflict.key` must appear as explicit entries in the mapping when `"*"` or `"*:body"` is used (loader defers the subset check; validate-pipeline re-runs it after expansion). Raw passthrough rejects any non-empty `cursor.fields` (`CursorRequiresExplicitFields`) and any `conflict.key` (`ConflictKeyNotInMapping`).

Universe-size cap: 4096 columns post-expansion → `WildcardUniverseTooLarge`.

**Body mapping `name = "*"`** (lowered to `Body { to = name }`)

Routes the row body into a single sink column. For relational sources the source builds the body as `Value::Json` via `air_elt_core::transform::build_body_json`; for Mongo sources the body is `Value::Custom(BsonObjectValue)`. The sink column type must accept the body shape (`Json` for the SQL sinks, schemaless for Mongo). Mixed mappings work: `id = "id"` next to `body = "*"` keeps `id` as a separate sink column AND includes `id` in the body. Multiple body-pack entries with distinct target columns are allowed. Duplicate target → `DuplicateSinkField`.

**Switch `field = { from = ..., switch = {...}, default = ... }`** (lowered to `TransformOp::Switch`)

Per-row value-to-value lookup. The `switch` inline table maps source-side keys (parsed against the source column's `DataType` — TOML inline-table keys are always strings, so `1 = "one"` and `"1" = "one"` both arrive as the key text `"1"` and are parsed as an integer when the source is `Int*`) to sink-side values.

```toml
[flow.<name>.mapping.status_label]
from = "status"
switch = { ACTIVE = "active", FINISHED = "finished" }
default = "unknown"
```

Behaviour:
- Hit → matched value (typed against the sink `DataType` for typed sinks; type-derived for schemaless).
- Miss → `default` (if set) or `Value::Null`.
- NULL source → `default` (if set) or `Value::Null`. The switch is NOT consulted on NULL.

**Value evaluation:** All switch values (RHS of each case and `default`) go through `Parser::parse_toml(&value)` → `Evaluator::create(&ctx).evaluate(&program)`. `Parser::parse_toml` (in `air_elt_expr_parse`) recursively maps TOML values to AST: strings flow through `Parser::parse` (which auto-detects expressions, interpolations, or plain literals via `detect`); ints/floats/bools become `LiteralValue`; tables become `Expr::Object`. Plain string literals like `"open"` are classified as literals and evaluated to `Value::Text("open")`. TOML integers produce `Value::Int64`; after evaluation, typed sinks run `ensure_sink_compatible` for auto-narrowing.

**Key canonicalization:** Switch keys are canonicalized through `Key::from_value` so that integer subtypes are unified: `Int8(1)`, `Int32(1)`, `BigInt(1)` all collapse onto the same `SwitchKey::Int(1)`. Float keys use `f64::to_bits` with NaN-bit-pattern normalisation (all NaNs collapse, `-0.0` collapses with `+0.0`). Sources whose declared `DataType` is `Json`/`Xml`/`Union`/`Custom` cannot host a switch — surfaces `SwitchUnsupportedSource`.

Sink type derivation:
- **Typed sink** (postgres/mysql/clickhouse/questdb): each evaluated RHS value is checked against the sink column's declared `DataType` via `ensure_sink_compatible`. Mismatch → `SwitchValueTypeMismatch`.
- **Schemaless sink** (mongo): each evaluated RHS value derives its own `DataType`. The set of observed types collapses via `core::types::collapse_union` into a single widened type when widening rules apply (Int8 ∪ Int32 → Int32; Text(5) ∪ Text(9) → Text(9)); otherwise the column type becomes `DataType::Union(...)`.

Validation rejects empty `switch = {}` and duplicate canonical keys (`SwitchDuplicateKey`). The match table is built once at validate time; runtime lookup is one `AHashMap` probe per row.

NULL fields are omitted from the body object — an all-NULL row produces `{}`.

Nesting depth limit: **64** (enforced inside `value_to_json`).

**Packed-JSON encoding (Debezium-compatible, no prefixes)**

| Canonical type | JSON representation |
|----------------|---------------------|
| Bool | bool |
| Int*/UInt* (≤ 2^53) | number |
| UInt64 > 2^53 / large Int64 | string |
| Float* | number; NaN/±Inf → `null` |
| BigInt / Decimal | string |
| Text | string |
| Bytes | bare hex string (no `hex:` prefix) |
| Date | `"YYYY-MM-DD"` |
| Timestamp | RFC3339 UTC (`...Z`) |
| Uuid | canonical string |
| Json | recursive |
| Xml | string (raw text) |
| Custom `mongodb.object_id` | 24-hex string |
| Custom `mongodb.javascript` | code as string |
| Custom `postgresql.hll` | base64 string |

Custom values delegate to `DynValue::to_json()`. New custom types must implement that method.

#### `default` value evaluation

All default values are expression-evaluated via `Evaluator::evaluate_expr_value()`. The TOML literal is classified as an expression, interpolation, or plain literal at parse time, then evaluated uniformly through the expression engine. After evaluation, the result is checked against the sink `DataType` via `ensure_sink_compatible`.

**Auto-narrowing:** TOML integer literals produce `Int64`; when the sink type is narrower (e.g. `Int8`), `try_narrow_numeric` checks the actual value fits and casts automatically. TOML float literals (`Float64`) auto-narrow to `Float32` the same way. For non-matching types, use an explicit cast expression: `default = "toInt8(42)"`.

**Examples:**

```toml
# Plain literals — auto-narrowed to sink type
default = 0              # Int64 → auto-narrows to Int8/Int16/etc. if value fits
default = 3.14           # Float64 → auto-narrows to Float32 if sink is Float32
default = false          # Bool
default = "n/a"          # Text (plain string, no expression detected)

# Expression defaults — evaluated via Parser::parse_toml + Evaluator
default = "env('DB_HOST', 'localhost')"
default = "toDate('2024-01-15')"
default = "toInt8(42)"                    # explicit cast for non-matching types
default = "if(isNull(env('OPT')), 'none', env('OPT'))"

# Interpolation defaults
default = "prefix_{env('SUFFIX')}"

# Structured defaults (JSON sink columns)
default = { a = 1 }
```

**Sink type constraints** — the evaluated value must be compatible with the sink column's `DataType`. Text values are length-checked against `size`, integers are range-checked, and so on. Incompatible values surface at validation time.

### `cursor`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `fields` | `[string]` | required | Cursor column(s), must be subset of mapping. With `"*"` or `"*:body"` the loader defers this check; the validate-pipeline re-runs it post-expansion against `direct.from`. |
| `order` | `"asc"` / `"desc"` | `"asc"` | Cursor direction |
| `interval` | Duration | `"1s"` | Idle interval between drain ticks |
| `jitter` | Duration | `min(interval, 5min)` | Deterministic per-flow startup offset. The runner sleeps `hash(flow.name) mod jitter` before the first tick, spreading concurrent flows across the cadence period so a fleet sharing one `interval` doesn't pile up on the same second-boundary. Max = `interval` (loader rejects `jitter > interval`). Set to `"0s"` to disable jitter entirely. |

### Duration format

All Duration fields accept two formats, routed by prefix:

**ISO 8601** (`P`/`p` prefix): `PT1H30M`, `P1DT2H`, `P1W`, `PT1.5S`. Years/months rejected. Serialization always uses ISO 8601.

**Human-time** (everything else): `1h30m`, `500ms`, `1.5s`, `1 hour`, `3 days`. Units must be in decreasing order (w > d > h > m > s > ms). Bare number = seconds (`42` = 42s).

## Validation rules

- Flow names unique across root + includes
- `batch-limit ≥ 1`
- `batch-limit × mapping_cols ≤ 60,000`
- `cursor.interval > 0` (zero interval causes spin-loop)
- `cursor.jitter ≤ cursor.interval` when explicitly set (`"0s"` accepted; default `min(interval, 5min)` always passes)
- `query-timeout > 0` when specified
- Cursor fields ⊆ mapping `from` columns
- `conflict.key` ⊆ mapping `to` columns (when `[flow.<name>.conflict]` is set)
- File size ≤ 16 MiB
- No absolute include paths
- Symlink loops detected via canonical path dedup