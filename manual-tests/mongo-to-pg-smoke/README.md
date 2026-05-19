# Manual smoke test: mongo → postgres

> ## ⚠ MANUAL CLEANUP REQUIRED AFTER EVERY RUN
>
> `run.py` deliberately leaves the Mongo and Postgres containers
> running and volumes mounted. When you are done iterating, run
> **`uv run --no-project scripts/cleanup.py`** — see the Cleanup
> section below. Skipping it leaves ports `27117` / `54322` bound and
> the next `run.py` picks up stale data.

Brings up Mongo 7 and Postgres 16 in compose, runs a continuous insert simulator against Mongo, and points an `air-elt` daemon at the pair.

Not wired into `cargo test`. Cleanup is explicit — see the section at the bottom.

## Prerequisites

- Container engine reachable through `docker` or `podman` compose v2 (auto-detected).
- `uv` on PATH.
- Rust toolchain (pinned via `rust-toolchain.toml`).
- Free local ports `27117` (mongo:7) and `54322` (postgres:16).

## Run

From this directory:

```
uv run --no-project scripts/run.py
```

`run.py` does, in order:

1. `compose up -d` and wait for both healthchecks.
2. `cargo build --release -p air-elt-app`.
3. Apply `init/migrate.sql` against `airdata` (creates `public.users`).
4. `air-elt migrate --config air-elt-config/config.toml` (creates the storage tables in `airstate`).
5. Spawn `scripts/{load,validate,stats}.py` in the background.
6. `air-elt run --config air-elt-config/config.toml` in the foreground. Ctrl-C to stop.

Override:

```
uv run --no-project scripts/load.py --rate 50 --duration 600
uv run --no-project scripts/run.py --duration 600
```

Default load: 20 docs/s for 360 s.

Load shape is **uniform**: one steady insert rate held for the whole
run, single collection, no spikes or warm-up ramp. The numbers in the
root `README.md` Benchmarks table were produced this way.

## Expected output

`run.py` keeps the foreground; `air-elt` logs batch writes to stdout.

A typical `validate.log` line:

```
[validate.py] 11:42:07 mongo=1240 pg=1232 lag_rows=8 pg_max_seq=1232 delta_pg=+95
```

`lag_rows` should oscillate near zero after the first couple of seconds.

At `AIR_ELT_LOG=debug`, `air-elt` emits `batch written rows=N` per micro-batch (from `crates/core/src/flow/runner.rs`).

## Log flag demos

Three knobs, one workload:

```
uv run --no-project scripts/run.py                              # default: async + text
AIR_ELT_JSON_LOGGING=true uv run --no-project scripts/run.py    # JSON lines
AIR_ELT_SYNC_LOGGING=true AIR_ELT_LOG=debug \
    uv run --no-project scripts/run.py                          # sync writes, batch-level logs
```

Ctrl-C between iterations — containers stay up.

## Side channels

Three tail-able logs:

- `logs/load.log` — insert simulator: rate, last seq.
- `logs/validate.log` — count + lag every 5s.
- `logs/stats.log` — CPU% + RSS for air-elt and both containers, TSV every 5s.

All three written by sibling scripts (`load.py`, `validate.py`, `stats.py`); `run.py` starts them.

## Cleanup

Containers and volumes survive across `run.py` invocations. Tear down when you're done:

```
uv run --no-project scripts/cleanup.py
```

Skipping this MANDATORY step leaves volumes mounted and ports 27117 / 54322 bound; the next `run.py` picks up stale documents.

## Caveats

- Port collision on 27117 (mongo) or 54322 (postgres): kill the other listener or edit `docker-compose.yml`.
- Healthcheck stuck: `docker compose ps`, then `docker compose logs <service>`.

## Layout

```
.
├── README.md
├── docker-compose.yml                 # mongo:7 + postgres:16, healthchecks
├── init/
│   ├── compose-init.sql               # creates airdata and airstate on first boot
│   └── migrate.sql                    # public.users target schema
├── air-elt-config/
│   ├── config.toml                    # air-elt root config
│   └── flows/users.toml               # mapping, cursor on _id, conflict=overwrite
└── scripts/
    ├── run.py                         # orchestrator (uv PEP-723)
    ├── cleanup.py                     # teardown
    ├── load.py                        # continuous insert simulator
    ├── validate.py                    # periodic count + lag reporter
    └── stats.py                       # CPU/RSS sampler (app + containers)
```
