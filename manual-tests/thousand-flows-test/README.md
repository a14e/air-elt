# Manual high-rate test: many flows, three sink topologies

> ## ⚠ MANUAL CLEANUP REQUIRED AFTER EVERY RUN
>
> `run.py` deliberately leaves containers and generated files in place.
> When you are done iterating, run **`uv run --no-project scripts/cleanup.py`**
> — see the Cleanup section at the bottom. Skipping it leaves the
> container stack and its published ports bound; the next run collides.
>
> ## ⚠ HEAVY ON RESOURCES
>
> Container count and port band are derived from `topology.yaml` at run
> time — see the formulas in the next section. Allocate **at least 16
> GB of RAM** to the container engine VM (Podman machine / Docker
> Desktop / colima) before running. Below that, sink containers OOM-kill
> once data starts flowing and the numbers skew. With podman on macOS,
> `run.py` checks the VM memory and warns if it's below 12 GiB.

The test brings up:

- `sources.count` Postgres source instances (default **10**).
- `sinks.postgres.count` Postgres sinks (default **5**).
- `sinks.clickhouse.count` ClickHouse sinks (default **2**).
- `sinks.questdb.count` QuestDB sinks (default **0** — disabled because
  QDB struggles past ~500 tables per instance under sustained load).
- 1 state Postgres for air-elt cursors.

Total container count = `sources.count + sinks.postgres.count +
sinks.clickhouse.count + sinks.questdb.count + 1` (default **18**).

Generated flow count = `sources.count × sources.tables_per_source`
(default **10 × 1000 = 10 000**). Each `(source, table)` pair is one
Air Elt flow, sink chosen by deterministic routing.

A single `air-elt` daemon is pointed at the whole matrix. The load
generator targets `~10 000 aggregate ops/s` at the default topology.

Flows are split across Postgres sinks (OLTP / data-mesh shape),
ClickHouse sinks (analytical / centralised-ELT shape), and QuestDB
sinks (time-series / mutable-via-DDL-dedup shape) per `topology.yaml`
sink counts. Within each sink kind, the first
`load.mutable_tables_per_source` flows carry mutation traffic
(INSERT … ON CONFLICT DO UPDATE) and the rest stay append-only — see
"Load shape" below.

Not wired into `cargo test`. Cleanup is explicit — see the section at
the bottom.

## Prerequisites

- Container engine reachable through `docker` or `podman` compose v2 (auto-detected).
- Container engine VM with **at least 16 GB of RAM** (Podman machine, Docker Desktop, colima — all expose this in their settings). Sustained working set across ~18 containers + `air-elt` lands in the 10-14 GiB band; CH especially needs headroom for merges and mark caches. Undersized VMs trigger OOM-kills that skew the numbers. Bump the podman machine with `podman machine set --memory 16384` and restart it.
- `uv` on PATH.
- Rust toolchain (pinned via `rust-toolchain.toml`).
- Free local ports: source pg `55100..55100 + sources.count - 1`, sink pg `55200..55200 + sinks.postgres.count - 1`, sink CH HTTP `8200..8200 + sinks.clickhouse.count - 1`, sink CH native `9200..9200 + sinks.clickhouse.count - 1`, QuestDB pg-wire `55400..55400 + sinks.questdb.count - 1`, QuestDB REST `9400..9400 + sinks.questdb.count - 1`, state pg `55300`. Edit `topology.yaml` `ports.*` if any band clashes locally.

## Run

From this directory:

```
uv run --no-project scripts/run.py
```

`run.py` does, in order:

1. On macOS + podman: vacuums the podman machine's journald (frees
   journald-backed page cache) and warns if VM memory is below 12 GiB.
2. `gen.py` — regenerates `docker-compose.yml`, `air-elt-config/`,
   `init/*`, `.env.generated` from `topology.yaml`.
3. `compose up -d` and waits for every container's healthcheck in parallel.
4. `cargo build --release -p air-elt-app`.
5. `air-elt migrate --config air-elt-config/config.toml`.
6. Spawns `scripts/{load,validate,stats}.py` in the background.
7. `air-elt run --config air-elt-config/config.toml` in the foreground.
   Ctrl-C, SIGTERM, or SIGHUP all trigger the same teardown path.

Override:

```
uv run --no-project scripts/run.py --duration 600       # stop air-elt after 10 min
uv run --no-project scripts/run.py --skip-gen           # reuse existing files
uv run --no-project scripts/run.py --skip-build         # reuse cargo artefacts
```

## What to look at

Three tail-able logs while the test runs:

- `logs/load.log` — load generator: total committed, observed
  ops/s, and per-source `deficit_ops` (positive = generator can't keep
  up; useful signal when scaling the target rate).
- `logs/validate.log` — count + max-id per source and per sink, every
  5 s. Signed `max_lag` so sink-ahead-of-source inversions are visible.
  Failing backends print `ERR` (not `0`) and are excluded from totals.
- `logs/stats.log` — aggregated CPU%/RSS per category (src_pg /
  sink_pg / sink_ch / sink_qdb / state_pg) plus the `air-elt` process
  itself, TSV every 5 s.

A typical `validate.log` block:

```
[validate.py] tick=23 t=01:55 stamp=12:01:55
  per_source     rows         max_id
  src_pg_00        120342       120342
  ...
  per_sink       rows         max_id
  sink_pg_00        24068        24068
  sink_ch_00        24067        24067
  TOTAL_SRC=1203410 TOTAL_SINK=1203220 max_src_id=120499 max_sink_id=120312 max_lag=187 err_rows=0
```

`max_lag` should oscillate near zero once flows are draining cleanly.
`stats.log` is the primary footprint signal — watch `app_rss_mb` as
flows ramp up.

## Benchmarks

Measured on a 5-CPU / 14 GiB Podman machine on Apple Silicon. Each
phase runs 60-120 seconds of sustained load after a 30-second
warm-up; the table reports steady-state numbers (avg) and the highest
sample seen (peak).

`sources` / `sinks` columns describe the **topology** that produced the
row — sources are always 10 PG-16 containers (one per `src_pg_NN`),
sinks are 5 PG-16 + 2 ClickHouse 24.3 (QuestDB disabled at 0). What
varies between phases is `tables_per_source` (= flows / 10), which
fans out per source. Sink routing is deterministic
`(src_idx × tables_per_source + tbl_idx) % 7` → ~equal distribution
across the 7 sinks. All flows are wired with
`conflict.strategy="overwrite"` (PG sinks) /
`ReplacingMergeTree(updated_at)` (CH sinks) because `load.py` emits
20 % `ON CONFLICT DO UPDATE` replay traffic that re-emits each replayed
row downstream via the cursor.

| Phase | Flows | Sources (PG-16) | Sinks (PG-16 + CH 24.3) | Achieved rows/s | air-elt CPU% (avg / peak) | air-elt RSS MB (avg / peak) | Final `max_lag` |
|---|---|---|---|---|---|---|---|
| 1 | 10 | 10 × 1 table | 5 PG (~2 flows each) + 2 CH (~1 flow each) | 95 / s | 0.4 / 2.4 | 24 / 26 | 0 |
| 2 | 100 | 10 × 10 tables | 5 PG (~15 flows each) + 2 CH (~14 flows each) | 360 / s | ~5 / 11 | ~32 / 33 | 0 |
| 3 | 1 000 | 10 × 100 tables | 5 PG (~143 flows each) + 2 CH (~143 flows each) | 1 980 / s | 8.6 / 8.7 | 158 / 180 | 0 |
| 4 | 10 000 | 10 × 1 000 tables | 5 PG (~1 429 flows each) + 2 CH (~1 428 flows each) | 9 000-11 000 / s | 63 / 82 | 432 / 523 | 0 (7 transient during a 1 s pg flap) |

Read across the rows:

- **air-elt scales sub-linearly with flow count.** 10× flows → 8× CPU,
  ~3× RSS. The single-process asyncio engine isn't the bottleneck up
  to 10 k flows / 10 k ops/s.
- **Container footprint scales with flows AND ops/s.** At Phase 4
  src_pg category sums ~4.2 GiB across 10 containers (~420 MiB each);
  sink_ch sums ~2.6 GiB (~1.3 GiB each — CH merges + mark cache);
  whole stack peaks at ~10 GiB of the 14 GiB VM allocation.
- **`max_lag=0` is achievable at every scale** once both ends stabilise.
  The Phase 4 transient blip was a single 1 s pg backend SIGPIPE →
  postmaster crash recovery; load.py reconnects automatically and
  air-elt's flow retry loop drains the gap within the next tick.

## Load shape

`load.py` opens **one persistent pipelined `psycopg.AsyncConnection`
per source** (default 10 connections total — no pools, no semaphores).
Each connection's coroutine drives all of its source's tables through
PG pipeline mode, so multi-thousand inserts/sec on one socket is
routine.

Two independent axes drive the workload:

**RPS axis (Pareto-ish, not uniform).** Per source,
`heavy_tables_per_source` tables (default 2) fire at `load.heavy_rps`
(default 250/s); the rest fire at `load.light_rps` (default 0.5/s).
At the defaults that's 20 hot + 9980 cold tables across 10 sources,
aggregate ~10 000 ops/s. Mirrors real systems — a handful of hot
tables and a long tail of cold ones.

**Mutation axis (OLTP-mutable vs analytical append-only).** Every row
is sent through `INSERT … ON CONFLICT (id) DO UPDATE`. For each
table, `update_pct`% of rows replay a recent id (CONFLICT path fires
on pg, causing the row to re-emit via `updated_at`); the rest use a
fresh client-side id (INSERT path). Mutable vs append-only at the
**sink** wiring level is governed by `mutable_tables_per_source`:

- **Mutable → Postgres sink**: flow .toml gets `[flow.<name>.conflict]
  key = ["id"] strategy = "overwrite"` so updates land idempotently
  on the BIGINT primary key.
- **Mutable → ClickHouse sink**: target table uses
  `ReplacingMergeTree(updated_at) ORDER BY id`. Dedup is engine-side
  and eventual — the validator queries with `FINAL` for these tables
  so pre-merge duplicates don't inflate the count.
- **Mutable → QuestDB sink**: target table is a WAL table partitioned
  by day with `DEDUP UPSERT KEYS(updated_at, id)`.
- **Append-only sinks**: no conflict block; sinks see fresh ids only.

All knobs live in `topology.yaml` — flip to uniform load by setting
`heavy_tables_per_source = tables_per_source`, or to pure append-only
by setting `mutable_tables_per_source: 0`. No code changes.

## `topology.yaml` knobs

The single operator-edited file. The most useful knobs:

- `sources.count` × `sources.tables_per_source` — total flow count.
  Default 10 × 1000 = 10 000.
- `sources.heavy_tables_per_source` — how many tables per source fire
  at `load.heavy_rps`; the rest fire at `load.light_rps`. Default 2 heavy / 998 light per source.
- `load.mutable_tables_per_source` — how many tables per source receive
  conflict/dedup wiring on the sink. Default half of
  `tables_per_source` (500). Set to 0 for pure append-only, set to
  `tables_per_source` for all-mutable.
- `load.update_pct` — share of generated rows that replay an existing
  id (forcing the ON CONFLICT DO UPDATE path on the source). Default 20.
- `sinks.postgres.count` + `sinks.clickhouse.count` +
  `sinks.questdb.count` — number of sink instances per type. Together
  they determine the routing modulus. Setting any one to 0 cleanly
  skips that backend.
- `ports.*` — port-band starts. Adjust if any band clashes locally.
- `load.heavy_rps` / `load.light_rps` — change the aggregate target.
- `load.batch_size` — rows per pipeline cycle per source. Default 100.
- `flow.batch_limit` — Air Elt's per-batch row cap. Defaults to 1000.
- `resources.pg_memory` / `resources.ch_memory` / `resources.qdb_memory`
  — informational compose memory caps.

After editing `topology.yaml`, re-run `scripts/run.py` (or
`scripts/gen.py` standalone to regenerate without spinning up).

## Cleanup (MANDATORY)

`run.py` deliberately does NOT tear down containers or remove generated
files — iteration between flag combinations is fast that way. When you
are done:

```
uv run --no-project scripts/cleanup.py
```

This runs `compose down -v --remove-orphans` and wipes:

- `logs/`, `.run-state/`
- `init/`, `air-elt-config/flows/`, `air-elt-config/config.toml`
- `docker-compose.yml`, `.env.generated`

If `compose down` fails (non-zero exit), `cleanup.py` aborts BEFORE
deleting the generated files, so you can investigate and retry without
losing the compose file that identifies the orphaned containers.

Skipping cleanup leaves the container stack and its published ports
bound; the next `run.py` will collide.

## Caveats

- Per-flow `.toml` count scales with `sources.count × sources.tables_per_source`. At 10 × 1000 = 10 000 files this is fine for the filesystem but slows IDE indexers — most editors offer a per-folder ignore.
- The validator uses `pg_class.reltuples` for cheap counts on BOTH source and sink Postgres, clamped with `GREATEST(reltuples, 0)`. Per-source / per-sink-pg row totals are *estimates* that catch up to exact values after pg's autovacuum settles. `max_id` is exact. CH counts for mutable tables use `count() FROM tbl FINAL` so pre-merge duplicates don't inflate the number.
- Port collision on any published port: kill the other listener or edit `topology.yaml` → `ports.*` and re-run.

## Layout

```
.
├── README.md
├── topology.yaml                      # operator-edited; everything else is generated
├── docker-compose.yml                 # GENERATED by gen.py
├── air-elt-config/
│   ├── config.toml                    # GENERATED
│   └── flows/                         # GENERATED (sources.count × tables_per_source files)
├── init/                              # GENERATED — per-backend SQL
│   ├── source_pg/srcNN.sql
│   ├── sink_pg/snkNN.sql
│   ├── sink_ch/snkNN.sql
│   ├── sink_qdb/snkNN.sql
│   └── state_pg/00-create-databases.sql
└── scripts/
    ├── gen.py                         # generator
    ├── run.py                         # orchestrator (podman vacuum + memory check on macOS)
    ├── load.py                        # async load gen (one psycopg AsyncConnection per source, pipeline mode)
    ├── validate.py                    # polling aggregator (chunked UNION ALL, loud errors)
    ├── stats.py                       # CPU/RSS sampler, engine-aware (docker / podman)
    └── cleanup.py                     # teardown
```
