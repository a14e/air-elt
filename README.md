# Air Elt

Air Elt is a lightweight EL(T) engine written in Rust. It moves data
between systems through declarative TOML/YAML flows and validates each
flow against the live targets before it starts.

- Single binary on stable Rust 1.95. No JVM, no auxiliary services, no
  orchestrator sidecar. `unsafe` is forbidden across the workspace, and
  every connector is covered end-to-end against real databases via
  testcontainers.
- Asynchronous worker per flow inside one process. Many flows live side
  by side on the same host.
- Micro-batch streaming. Each tick pulls a batch from the source, runs
  the compiled transform, writes to the sink, persists the cursor, and
  loops. Sub-second cycles by default.
- Pre-deploy validation: connectivity and authentication, schema and
  field-type compatibility, cursor and mapping structure, sample probes.
- Handles schematic and schemaless systems under the same flow shape.
  PostgreSQL and MySQL expose authoritative catalogues; MongoDB and
  mongo-cdc derive per-cell types at row time. Compatibility checks
  adapt to whichever side carries a contract.
- Thin transform layer. Type coercions happen at boundaries; heavier
  reshaping belongs upstream or downstream, not inside the data path.
- Simple monolithic shape. The process itself is stateless — cursor
  state lives in the configured storage — so the binary is easy to
  configure, deploy, and replace.
- GitOps-first. Flows are plain config files in your repo, reviewed in
  PRs and rolled out like any other config.

## Running

Local prerequisites: Rust 1.95 (pinned via `rust-toolchain.toml`) for
the binary itself, and a container engine (Docker or Podman) for the
testcontainers-backed integration tests. The manual benchmark scaffolds
under `manual-tests/` additionally need `uv` to launch the PEP-723
Python helpers.

Build with `cargo build --release`.

```bash
# Validate the config against live sources, sinks, and storages
air-elt validate --config config.toml

# Apply storage migrations
air-elt migrate --config config.toml

# Daemon mode (graceful shutdown on SIGTERM)
air-elt run --config config.toml

# One-shot drain and exit
air-elt run --once --config config.toml
```

TOML (`.toml`) and YAML (`.yml`, `.yaml`) are accepted; the format is
detected per file by extension. Without `--config`, the CLI probes
`./config.toml`, then `./config.yml`, then `./config.yaml`.

Logging is controlled by environment variables. `AIR_ELT_LOG` sets the
level (defaults to `info`). `AIR_ELT_JSON_LOGGING=true` switches the
formatter to single-line JSON for log shippers. `AIR_ELT_SYNC_LOGGING=true`
flushes each record synchronously, which is useful when chasing crashes.

## Configuration

A flow names a source, a sink, optional storage for cursor state, and a
mapping from source columns to sink columns.

```yaml
sources:
  - name: orders-pg
    type: postgres
    config:
      url: "postgres://app:app@db.internal:5432/orders"

sinks:
  - name: orders-mongo
    type: mongodb
    config:
      url: "mongodb://app:app@mongo.internal:27017"
      database: orders

storages:
  - name: cursors
    type: postgres
    config:
      url: "postgres://app:app@db.internal:5432/airstate"

flow:
  orders:
    source: orders-pg
    sink: orders-mongo
    storage: cursors
    from: public.orders
    to: orders

    cursor:
      fields: [updated_at, id]
      order: asc
      interval: 1s

    mapping:
      _id: id
      updated_at: updated_at
      total_cents: total_cents

    conflict:
      key: [_id]
      strategy: overwrite
```

URLs accept `${VAR}` or `${VAR:default}`; references resolve from process
env at load time, then from the `secrets` mapping.

## Supported backends

- **PostgreSQL** — source, sink, cursor storage.
- **MySQL / MariaDB 10.7+** — source, sink, cursor storage.
- **CockroachDB** — source, sink, cursor storage (Postgres wire protocol).
- **ClickHouse** — sink (append-only, no deletes).
- **QuestDB** — sink (append-only, DDL-level `DEDUP UPSERT KEYS`).
- **MongoDB** — source, sink, cursor storage.
- **mongo-cdc** — source (change streams; emits inserts, updates, deletes).

## Roadmap

- **Operational** — metrics (Prometheus / OpenTelemetry), Vault for secrets,
  dead-letter queues, soft delete propagation.
- **Targets** — Kafka, RabbitMQ, NATS, RocketMQ; S3 and Iceberg; Redis;
  vector databases (Qdrant and similar).
- **Engine** — wider canonical type coverage including precision-aware
  numerics; full-text search engines (Elasticsearch, Manticore,
  Meilisearch); expression-level transforms.

## Benchmarks

Numbers from the scaffolded manual tests in this repo. Reproducible
locally; the scaffolds spin up the backends in containers, run the
workload, and sample resources.

Host: Apple M2 Pro, 12 cores, 32 GB, macOS 26.2, Podman 5.8.

### Single flow, mongo → postgres

Scaffold: [`manual-tests/mongo-to-pg-smoke/`](https://github.com/a14e/air-elt/tree/main/manual-tests/mongo-to-pg-smoke).
A sanity smoke test — one flow, 10 columns (text / bigint / boolean /
integer / float / numeric / jsonb / timestamptz / ObjectId), cursor on
`_id`. Load is uniform: one steady insert rate, no warm-up ramp. Not
the high-rate path — see the thousand-flows benchmark below for sustained
throughput numbers.

Run it:

```
cd manual-tests/mongo-to-pg-smoke
uv run --no-project scripts/run.py --duration 330 --load-rate <N>
```

App CPU is reported as a percentage of one core (100% = one fully busy
core). Numbers come from cumulative `cpu_times()` deltas, not
instantaneous sampling.

### Many flows: scaling from 10 to 10 000 in one process

Scaffold: [`manual-tests/thousand-flows-test/`](https://github.com/a14e/air-elt/tree/main/manual-tests/thousand-flows-test).
Ten Postgres source containers fan out to five Postgres + two ClickHouse
sinks. Every flow is OLTP-mutable: `load.py` emits `INSERT … ON CONFLICT
DO UPDATE` with 20 % replay traffic so each flow exercises the sink-side
conflict path (PG `conflict.strategy = "overwrite"`, CH
`ReplacingMergeTree(updated_at)`). Pareto shape: 2 heavy tables per
source plus a long tail of light tables.

Host: 5-CPU / 14 GiB Podman machine on Apple Silicon, Podman 5.8.

| Flows  | Sources × Tables | Sinks (PG + CH) | Achieved RPS | App CPU avg / peak | App RSS avg / peak | Final `max_lag` |
|-------:|------------------|-----------------|-------------:|-------------------:|-------------------:|----------------:|
| 10     | 10 × 1           | 5 + 2           |          95  |   0.4 % / 2.4 %    |    24 / 26 MB      | 0               |
| 100    | 10 × 10          | 5 + 2           |         360  |   ~5 % / 11 %      |   ~32 / 33 MB      | 0               |
| 1 000  | 10 × 100         | 5 + 2           |       1 980  |   8.6 % / 8.7 %    |   158 / 180 MB     | 0               |
| 10 000 | 10 × 1 000       | 5 + 2           | 9 000-11 000 |    63 % / 82 %     |   432 / 523 MB     | 0 (7 transient) |

Reading the table:

- **air-elt scales sub-linearly.** 10× flows costs ~8× CPU and ~3× RSS.
  The single-process asyncio engine isn't the bottleneck up through
  10 000 flows / 10 000 ops/s.
- **Routing is deterministic.** Sink slot is
  `(src_idx × tables_per_source + tbl_idx) mod 7`; flows distribute
  ~equally across the 7 sinks regardless of `tables_per_source`.
- **`max_lag = 0` is achievable at every scale.** The Phase 4 transient
  blip was a single 1-second pg backend SIGPIPE → postmaster crash
  recovery; `load.py`'s reconnect supervisor reopened the connection
  automatically and air-elt's flow-level retry drained the gap within
  the next tick.
- **Load shape is one persistent pipelined `psycopg.AsyncConnection`
  per source** (10 connections total). PG pipeline mode lets a single
  TCP socket sustain 10 000+ ops/s per source — no pools, no semaphores,
  no producer/consumer queue.

Run any row yourself by editing `topology.yaml`:

```
cd manual-tests/thousand-flows-test
# edit topology.yaml (sources.count, tables_per_source, load.heavy_rps, ...)
uv run --no-project scripts/run.py --duration 240
```

Startup cost (not in the table above): `gen.py` (sub-second),
`compose up` + healthcheck wait (5-30 s depending on PG/CH cold-start),
`cargo build --release` (~45 s if not cached), `air-elt migrate` against
the state DB (~1 s).

QuestDB is supported as a third sink kind in the generator
(`sinks.questdb.count`). The scaffold pins the QDB image to **8.2.3**:
earlier versions (notably 8.1.1) mis-type extended-protocol bind
parameters as STRING for every non-STRING/non-LONG column, which makes
Air Elt's `validate_access` dry-run probe fail with a cryptic
"inconvertible types" error. The regression is covered by
`crates/sinks/questdb/tests/oltp_twelve_columns.rs` against the
12-column OLTP shape so a future image bump cannot silently regress.

## Contributing

Issues and feature requests are welcome. Bug reports with reproduction
steps and a minimal config carry the report.

Code changes are written by the core team. Pull requests from outside are
likely to be closed; if you want a feature or a fix shipped, file an
issue and the team will pick it up.

## License

Licensed under Apache 2.0 OR MIT. See `LICENSE-APACHE` and `LICENSE-MIT`.
