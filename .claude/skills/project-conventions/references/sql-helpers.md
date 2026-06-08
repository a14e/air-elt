# SQL helpers (full reference)

All dynamic SQL identifiers must go through these helpers. Raw `format!` quoting is forbidden.

- **`air_elt_commons::identifier`** — db-agnostic validation primitives + `IdentifierError`.
- **`air_elt_commons_pg::identifier`** — pg quoting (`"`).
- **`air_elt_commons_mysql::identifier`** — mysql quoting (backtick).
- **`IdentifierError → RuntimeError`** via `impl From` in `core::error` — use `?` directly.

Source-side type resolution lives in `commons-pg::pg_type` / `commons-mysql::mysql_type` (native ↔ canonical `DataType`). Notable quirks: pg accepts `timestamptz` only (naive `timestamp` rejected); mysql `tinyint(1)` → `Bool`, other signed tinyints → `Int8` (was `Int16` before AIR-22), `datetime` rejected (only `timestamp` accepted, UTC).

NULL binding goes through `commons-pg::null_bind` / `commons-mysql::null_bind` (extracted helper) on the source side. On the sink side use **`commons-pg::sink_bind::bind_value_separated`** / **`commons-mysql::sink_bind::bind_value_separated`** for binding a `Value` inside a sqlx `Separated` chain. They are shared between the insert (`push_values`) and delete (`push_tuples` for the `(c1,c2) IN ((...))` predicate) paths — do not reimplement per-Value-variant binding inline.

Pool construction goes through `commons-pg::pool` / `commons-mysql::pool`, both consuming `air_elt_commons::pool_timeouts::PoolTimeouts`. They wire UTC time-zone + statement-timeout pragmas. Defaults: connect 5s, acquire 10s, idle 300s, max_lifetime 1800s, statement 30s, max_connections 5.

Schema introspection is `commons-pg::schema::fetch_schema` / `commons-mysql::schema::fetch_schema`. Both read `character_maximum_length`; mysql additionally reads `column_type` for `tinyint(1)` discrimination.

Each connector owns its `sql_statements.rs`. Bind values via sqlx `$N` + `query.bind()` / `QueryBuilder::push_bind` — never interpolate values into SQL.

## Postgres dialect flag (`air_elt_commons_pg::Dialect`)

The Postgres connector crates serve both `type = "postgres"` and `type = "cockroachdb"`. `PgSourceConfig`/`PgSinkConfig`/`PgStorageConfig` carry a `dialect: Dialect` set by the factory (`PgXxxFactory::postgres()` vs `::cockroach()`); the field is `#[serde(skip)]` so users never touch it. The dialect flag drives only:

- `Dialect::excludes_type(&DataType)` — reject `Xml` columns at `validate_access` for Cockroach (no XML type there).
- `air_elt_commons_pg::retry::with_serialization_retry(dialect, op)` — **mandatory** wrapper around any write-path statement when adding new code paths. On Postgres it's a single-shot pass-through (zero behaviour change). On Cockroach it retries on SQLSTATE `40001 RETRY_SERIALIZABLE` with exponential backoff up to `MAX_ATTEMPTS = 10` total executions (base 50ms, capped at 2s). Reuse this helper rather than rolling your own retry loop.

Conflict resolution emits the standard `INSERT … ON CONFLICT (key) DO …` SQL on both dialects. Cockroach's native `UPSERT` is deliberately not used: it silently uses the primary key as the conflict arbiter regardless of any user-declared `conflict.key`, which would mask misconfiguration if a user pointed at a UNIQUE secondary index instead.

Cockroach storage migrations live in `migrations/storage-cockroachdb/` (byte-identical copies of `storage-postgres/`); `PgStorage::migrate()` branches on `self.dialect` between the two `sqlx::migrate!` paths.
