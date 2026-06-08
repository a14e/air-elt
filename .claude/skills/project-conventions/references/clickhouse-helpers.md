# ClickHouse helpers (full reference)

ClickHouse is sink-only today; `commons-clickhouse` carries the helpers shared with any future CH source. Reuse these — do not roll up ad-hoc HTTP / type-parsing per call site.

- **`commons-clickhouse::client`** — `reqwest::Client` wrapper plus `ChClientConfig` (URL, database, `user`, `password`, `PoolSettings`). `user` and `password` are required strings — use `""` for the authless variant on CH instances with `<networks>` open for the `default` user. Both auth headers (`X-ClickHouse-User` / `X-ClickHouse-Key`) are always emitted regardless of value; no "skip header when empty" branching. `ping()`, `query_text()`, `insert_row_binary()`. We use `reqwest` directly rather than the `clickhouse` 0.13 crate because that crate's typed `Client::insert::<T: Row>` API doesn't fit dynamic `Vec<Value>` batches.
- **`commons-clickhouse::identifier`** — backtick quoting (CH shares MySQL's backtick syntax). `quote_ident`, `quote_qualified`, `quote_columns`, `split_qualified`.
- **`commons-clickhouse::ch_type_parser`** — recursive parser for `system.columns.type` strings. Returns `ParsedType { data_type, nullable }`. `Nullable(T)` strips onto the `nullable` flag; `LowCardinality(T)` strips transparently. Composite shapes (`Array`, `Tuple`, `Map`, `Nested`, geo) map onto `DataType::Json`.
- **`commons-clickhouse::schema`** — `fetch_schema(client, table)` runs `SELECT name, type FROM system.columns … FORMAT JSON` and folds the result into a canonical `Schema`.
- **`commons-clickhouse::row_binary`** — `encode_value(out, &Field, &Value)` writes one column-cell into a `RowBinary` byte buffer. Handles `Nullable` flag bytes, UTF-8 string LEB128 length prefix, CH's mixed-endian UUID layout, Date as `u16` days, DateTime as `u32` seconds (UTC, no TZ), `Decimal` as fixed-width signed LE (width by precision: ≤9=i32, ≤18=i64, ≤38=i128, ≤76=i256).
- **`commons-clickhouse::types`** — the CH `DynType`/`DynValue` registry:
  - `aggregate_state` — `ChAggregateStateType { fn_name, arg_types, simple }` + opaque-bytes `ChAggregateStateValue`. `kind()` is `clickhouse.aggregate.<snake_fn>` (leak-interned at first observation per process — bounded by user-declared columns).
  - IPv4 / IPv6 are canonical (`DataType::Ipv4` / `DataType::Ipv6`, `Value::Ipv4(Ipv4Addr)` / `Value::Ipv6(Ipv6Addr)`); CH `IPv4` columns encode as LE u32 and `IPv6` as 16 BE octets inside `commons-clickhouse::row_binary`.
  - `fixed_string` — `ChFixedStringType { size }` + bytes carrier. Cross-canonical to/from `Bytes(N)`.
  - `enum_` — `ChEnum8Type` / `ChEnum16Type` (variants table) + `ChEnumValue { name }`. Cross-canonical to/from `Text` (variant name).
  - `int128` — `ChInt128Type` / `ChUInt128Type` + `ChInt128Value(i128)` / `ChUInt128Value(u128)`. 16-byte LE. Cross-canonical to/from `BigInt`.
  - `int256` — `ChInt256Type` / `ChUInt256Type` + `ChInt256Value { le_bytes: [u8; 32] }` / `ChUInt256Value { le_bytes: [u8; 32] }`. 32-byte LE two's-complement. Cross-canonical to/from `BigInt`. Helpers: `bigint_to_le32`, `le32_to_bigint`, `biguint_to_le32`.

**Custom `kind` values shipped**: `clickhouse.fixed_string`, `clickhouse.enum8`, `clickhouse.enum16`, `clickhouse.int128`, `clickhouse.uint128`, `clickhouse.int256`, `clickhouse.uint256`, `clickhouse.aggregate.<fn>` (e.g. `clickhouse.aggregate.quantiles_t_digest`, `clickhouse.aggregate.quantiles_d_d_sketch`). IPv4 / IPv6 used to live here as `clickhouse.ipv4` / `clickhouse.ipv6`; they have been promoted to canonical `DataType::Ipv4` / `DataType::Ipv6` (AIR-88).
