# Air Elt

Declarative ELT pipelines in Rust. Micro-batch + drain data movement between systems with strict validation on startup and `tracing` observability everywhere.

The architectural overview, config format, and conventions live in the `.claude/skills/` folder — see `air-elt-overview`, `rust-guidelines`, and `project-conventions` (loaded automatically when working with Claude Code in this repo). Rules for contributors are in `AGENTS.md`.

## Building

```bash
cargo build --release
cargo test --workspace
```

Rust **1.95** (stable) is pinned via `rust-toolchain.toml`.

## Running

```bash
# Validate a config (connects to all declared sources/sinks/storages)
cargo run -p air-elt-app -- validate --config examples/pg-to-mongo/config.toml

# Apply storage migrations
cargo run -p air-elt-app -- migrate --config examples/pg-to-mongo/config.toml

# Daemon mode — micro-batch + drain, graceful shutdown on SIGTERM
cargo run -p air-elt-app -- run --config examples/pg-to-mongo/config.toml

# One-shot drain
cargo run -p air-elt-app -- run --once --config examples/pg-to-mongo/config.toml

# Shorthand: no subcommand → daemon mode with ./config.toml
cargo run -p air-elt-app
```

Both TOML (`.toml`) and YAML (`.yml` / `.yaml`) are supported equally — pick whichever you prefer. The format is detected per file by extension, and mixing files of different formats inside the same `include` graph is allowed (a TOML root may include a YAML flow file and vice versa). When invoked without `--config`, the CLI probes `./config.toml`, then `./config.yml`, then `./config.yaml`, and uses the first one it finds.

## Tests

E2E tests need a PostgreSQL instance. Two options:

- **Local**: install podman — no env vars needed. `pg_pool()` auto-detects the podman socket (including the macOS `podman machine` layout under `$TMPDIR/podman/*-api.sock`) and spins up ephemeral containers. Docker Desktop works via the default `/var/run/docker.sock` path.
- **External / CI**: set `AIR_ELT_TEST_PG_URL=postgres://…`. Tests create unique sandbox schemas and clean them up on handle drop. Orphaned schemas older than 24 h are self-healed on startup.

## Environment variables

- `AIR_ELT_TEST_PG_URL` — external postgres for e2e tests. CI uses this; local dev almost never needs it.
- `AIR_ELT_TEST_COCKROACHDB_URL` — external CockroachDB instance (`postgres://root@host:26257/defaultdb?sslmode=disable` shape). Optional locally — `cockroach_pool()` falls back to a `cockroachdb/cockroach:v25.1.0 start-single-node --insecure` testcontainer when unset. CI sets it.
- `DOCKER_HOST` — override socket auto-detection for testcontainers. Usually unset.
- `AIR_ELT_LOG` / `RUST_LOG` — logging level (`info` by default). **`debug` is only safe for short diagnostic sessions** — it logs full SQL per batch, which is tens of KB per line at default `batch_limit = 1024`. Do not leave `debug` on in production. `trace` is an even firmer don't-for-prod.
- Any `${VAR}` reference inside the config file is expanded from process env at load time; defaults are spelled `${VAR:default}`. Avoid embedding secrets in command-line arguments — they appear in `ps` output. Use the config's `[secrets]` section or a dedicated `${VAR}` with a restricted env.

## CockroachDB

CockroachDB is supported as `type = "cockroachdb"` for sources, sinks, and storages. Under the hood it reuses the Postgres connector crates with a `Dialect::Cockroach` flag selecting the divergent code paths:

- write paths automatically retry on `40001 RETRY_SERIALIZABLE` (Cockroach defaults to SERIALIZABLE isolation),
- `XML` columns are rejected upfront at validation time (Cockroach has no XML type),
- migrations live in `migrations/storage-cockroachdb/` (byte-identical to the Postgres ones — `TEXT`/`JSONB`/`TIMESTAMPTZ` are all supported); the migrator skips `pg_advisory_lock` since CockroachDB doesn't implement it,
- conflict resolution stays on the standard `INSERT … ON CONFLICT (key) DO …` path on both engines. CockroachDB's native `UPSERT` is intentionally not used — it silently treats the primary key as the conflict arbiter regardless of the user-declared `conflict.key`, which can mask misconfiguration.

Connection string is the standard Postgres URL pointing at port 26257, e.g. `postgres://root@host:26257/mydb?sslmode=disable`. See `examples/pg-to-cockroachdb/` for a working flow.

## Bootstrap a CDC pipeline

The `mongo-cdc` source streams MongoDB change events (insert / update / replace / delete) and emits `Upsert` / `Delete` rows to any sink. It needs a **replica-set** Mongo deployment — change streams cannot run on standalone mongod.

A from-scratch bootstrap pairs two flows over the same collection:

1. A **snapshot** flow with `[[sources]] type = "mongodb"` (cursor-driven, full collection scan).
2. A **cdc** flow with `[[sources]] type = "mongo-cdc"` (change-stream driven, picks up new mutations including DELETEs).

Run them in parallel until the snapshot finishes, then disable the snapshot flow — the cdc flow keeps the table fresh from the oplog. Choose `mode = "post-image"` (requires `changeStreamPreAndPostImages` enabled on the collection) or `mode = "lookup-on-update"` (one extra `find` per batch, no server-side flag) per flow.

A working example sits at `examples/mongo-cdc-to-pg/`. See the `config-format` skill for the full field reference.

## Configs and secrets

`${VAR}` / `${VAR:default}` placeholders are resolved before TOML parse. Lookup order: process env → config `[secrets]` map → default clause → error. See `examples/pg-to-mongo/config.toml`.

Storage schema placement is **not** a config field in MVP — if you need the cursor table in a non-default schema, put `?options=-c%20search_path%3D<schema>` in the storage URL. libpq applies that to every new pool connection, so migrations and runtime queries agree.

## License

Licensed under either of

- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work shall be dual licensed as above, without any additional terms or conditions.
