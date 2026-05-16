---
name: config-format
description: Complete reference for the Air Elt TOML/YAML config format — all sections, fields, defaults, and validation rules. Load before editing config files, config structs, or the loader.
user-invocable: false
---

# Config format

Air Elt accepts both TOML (`.toml`) and YAML (`.yml`/`.yaml`). Format is detected per file by extension; mixing formats inside one include graph is allowed. The shape is identical — a TOML `[[sources]]`/`[flow.<name>]`/inline-table maps mechanically to a YAML list / nested map / nested mapping under the same keys. All examples below are TOML; translate to YAML by that mapping when needed. Multi-word keys use **kebab-case** (`batch-limit`, `operation-timeout-secs`) in both formats.

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

## `[[sources]]` / `[[sinks]]` / `[[storages]]`

| Field | Type | Required | Description |
|-------|------|:--------:|-------------|
| `name` | string | yes | Unique identifier |
| `type` | string | yes | Connector kind (`"postgres"`, `"cockroachdb"`, `"mysql"`, `"mongodb"`, `"mongo-cdc"` (source only), `"clickhouse"` (sink only)) |
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

### ClickHouse sink config (`[[sinks]] type = "clickhouse"`)

ClickHouse is **sink-only** today (no source, no storage). The sink declares `supports_deletes() = false`: the runner drops `RowOp::Delete` rows before `write_batch`, the validation pipeline skips `validate_delete_access`, and CDC sources may pair with it without a mandatory `[flow.<name>.conflict]` block (append-only ingest). The MergeTree family has no cheap `DELETE`/`UPDATE`; emulating deletes via `ALTER TABLE … DELETE` mutations is intentionally not supported.

INSERTs use the HTTP `RowBinary` format. Authentication is over standard CH `X-ClickHouse-User` / `X-ClickHouse-Key` headers.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `url` | string | required | HTTP endpoint URL (e.g. `http://localhost:8123`). Use `https://` for TLS. No trailing slash. |
| `database` | string | required | Default database. Applied as `X-ClickHouse-Database`; flow `to` may still be `db.table` qualified to override per flow. |
| `user` | string | none | Optional username. |
| `password` | string | none | Optional password. Pair with `[secrets]` to avoid leaking it in the config file. |
| `connect-timeout` | Duration | `"5s"` | TCP connect timeout. |
| `idle-timeout` | Duration | `"5m"` | Idle HTTP connection lifetime. |
| `request-timeout` | Duration | `"30s"` | Whole-request cap (connect + send + server compute + body download). CH has no per-statement timeout exposed over HTTP. |
| `max-connections` | u32 | 5 | HTTP pool size cap. |

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
blob_out = { from = "blob_in",  default = "hex:00" }

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
| `truncate` | bool | `false` | Opt the column into narrowing conversions: text/bytes shrink (UTF-safe for text), integer/float saturate to target's max/min, decimal scale drop, json/xml → `text(n)` serialize. Forbidden combinations (`Json → Json`, `Xml → Xml`, UUID truncations, `Date → Timestamp`) remain rejected. |
| `default` | scalar / table | none | Fallback value substituted when the source value is `Null` and when `switch` produces no match. Permits mapping a nullable source into a `NOT NULL` sink. On the Direct path validation rejects `default` if the source column is `NOT NULL`. The literal is parsed against the resolved sink `DataType` (see grammar below). |
| `switch` | inline table | none | Value-to-value lookup. Keys (inline-table keys — always strings in TOML) are parsed against the source column's `DataType`; values are parsed against the sink column's `DataType` (or contribute to union-collapse for schemaless sinks). Output: the matched value, or `default` on miss / NULL input, or `Value::Null` if no `default`. See **Switch** below. |

`#[serde(deny_unknown_fields)]` rejects any additional keys at parse time on the long form — including a stray `to` field (the map key already carries it).

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

Key matching is type-canonical, NOT string-canonical: `Int8(1)`, `Int32(1)`, `BigInt(1)` all collapse onto the same `SwitchKey::Int(1)`. Float keys use `f64::to_bits` with NaN-bit-pattern normalisation (all NaNs collapse, `-0.0` collapses with `+0.0`). Sources whose declared `DataType` is `Json`/`Xml`/`Union`/`Custom` cannot host a switch — surfaces `SwitchUnsupportedSource`.

Sink type derivation:
- **Typed sink** (postgres/mysql/clickhouse/questdb): each RHS literal is parsed against the sink column's declared `DataType` via the same parser used by `default`. Mismatch → `SwitchValueTypeMismatch`.
- **Schemaless sink** (mongo): RHS literals are parsed untyped → each produces a `(Value, DataType)` pair. The set of observed `DataType`s collapses via `core::types::collapse_union` into a single widened type when widening rules apply (Int8 ∪ Int32 → Int32; Text(5) ∪ Text(9) → Text(9)); otherwise the column type becomes `DataType::Union(...)`.

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
| `fields` | `[string]` | required | Cursor column(s), must be subset of mapping. With `"*"` or `"*:body"` the loader defers this check; the validate-pipeline re-runs it post-expansion against `direct.from`. |
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