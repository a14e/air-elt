# Air Elt

Declarative ELT pipelines in Rust. Micro-batch + drain data movement between systems with strict validation on startup and `tracing` observability everywhere.

The architectural overview, config format, and conventions live in the `.claude/skills/` folder — see `air-elt-overview`, `rust-guidelines`, and `project-conventions` (loaded automatically when working with Claude Code in this repo). Rules for contributors are in `AGENTS.md`.

## Building

```bash
cargo build --release
cargo test --workspace
```

Rust **1.90** (stable) is pinned via `rust-toolchain.toml`.

## Running

```bash
# Validate a config (connects to all declared sources/sinks/storages)
cargo run -p air-elt-app -- validate --config examples/pg-to-pg/config.toml

# Apply storage migrations
cargo run -p air-elt-app -- migrate --config examples/pg-to-pg/config.toml

# Daemon mode — micro-batch + drain, graceful shutdown on SIGTERM
cargo run -p air-elt-app -- run --config examples/pg-to-pg/config.toml

# One-shot drain
cargo run -p air-elt-app -- run --once --config examples/pg-to-pg/config.toml

# Shorthand: no subcommand → daemon mode with ./config.toml
cargo run -p air-elt-app
```

## Tests

E2E tests need a PostgreSQL instance. Two options:

- **Local**: install podman — no env vars needed. `pg_pool()` auto-detects the podman socket (including the macOS `podman machine` layout under `$TMPDIR/podman/*-api.sock`) and spins up ephemeral containers. Docker Desktop works via the default `/var/run/docker.sock` path.
- **External / CI**: set `AIR_ELT_TEST_PG_URL=postgres://…`. Tests create unique sandbox schemas and clean them up on handle drop. Orphaned schemas older than 24 h are self-healed on startup.

## Environment variables

- `AIR_ELT_TEST_PG_URL` — external postgres for e2e tests. CI uses this; local dev almost never needs it.
- `DOCKER_HOST` — override socket auto-detection for testcontainers. Usually unset.
- `AIR_ELT_LOG` / `RUST_LOG` — logging level (`info` by default). **`debug` is only safe for short diagnostic sessions** — it logs full SQL per batch, which is tens of KB per line at default `batch_limit = 1024`. Do not leave `debug` on in production. `trace` is an even firmer don't-for-prod.
- Any `${VAR}` reference inside the config file is expanded from process env at load time; defaults are spelled `${VAR:default}`. Avoid embedding secrets in command-line arguments — they appear in `ps` output. Use the config's `[secrets]` section or a dedicated `${VAR}` with a restricted env.

## Configs and secrets

`${VAR}` / `${VAR:default}` placeholders are resolved before TOML parse. Lookup order: process env → config `[secrets]` map → default clause → error. See `examples/pg-to-pg/config.toml`.

Storage schema placement is **not** a config field in MVP — if you need the cursor table in a non-default schema, put `?options=-c%20search_path%3D<schema>` in the storage URL. libpq applies that to every new pool connection, so migrations and runtime queries agree.

## License

Licensed under either of

- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work shall be dual licensed as above, without any additional terms or conditions.
