# Metrics

The crate `air-elt-monitoring` (`crates/monitoring`) is the single owner of Prometheus instrumentation. Every other crate goes through its `MonitoringManager` — direct calls to `prometheus::*` or to `sketches-ddsketch` are forbidden outside `monitoring`.

The contract has two halves: a **manager** (built once from config at app start, owns the registry and the HTTP server) and **recorders** (cheap typed handles threaded into flows / locks / connectors). Recorders hide all label arity, all hot-path string handling, and the enabled-vs-disabled split.

## When to add a metric

Air Elt's brief requires that **every long-running process is observable**. When you add a new feature that introduces a code path of one of the following shapes, add a metric for it:

1. A loop, retry, or backoff that an operator might want to confirm is making progress.
2. A blocking acquire of a shared resource (semaphore / pool / channel) — emit queue depth and active count as time-integrating gauges.
3. A user-visible failure surface — emit a counter keyed by error stage + kind.
4. A timed operation whose tail latency matters for SLOs.

For (1) and (3) use a plain `IntCounter`/`IntGauge` from `prometheus`. For (2) prefer the project's `TimeIntegratingGauge` — read time-averaged values via `rate(metric[window])`. For (4) use the project's custom `Summary` (sliding-window DDSketch).

## Recorder pattern

Every recorder is `pub struct Foo { inner: Option<Arc<FooInner>> }`. `None` = disabled (monitoring off, or validation-time stub). Enabled = pre-baked label values + pre-extracted instrument handles. Disabled-mode method bodies short-circuit on `Option::is_none()`; cost in the disabled build is roughly one discriminant load.

Hot-path discipline:

- `slot.observe(value)` for `Summary` — a `SummarySlot` carries the parent `Summary` (Arc-clone) and an index into the parent's `Mutex<Vec<SummarySlotInner>>`. No per-slot Arc, no nested mutex.
- `slot.set(value)` / `slot.add(delta)` for `TimeIntegratingGauge` — same index-into-parent shape; Kahan accumulator in the inner.
- Pre-extracted `IntCounter` / `IntGauge` handles for the labelled counter vecs (`rows_total`, etc.). The labels never change for a given recorder.
- `errors_total` is **lazy**: the family is registered as `IntCounterVec`, but no child is materialised until `record_error(stage, kind)` actually fires. Stages × kinds is a wide combinatorial space; pre-extraction was tried and removed (validator-flagged combinatorial explosion).

`MonitoringManager` itself is **mutable construction-time** (`&mut self` on `flow_recorder`, `lock_recorder`, `set_lock_max`, `pool_stats_recorder`, `set_counts`). It carries an `AHashMap` cache keyed by labels so repeated mints return the same recorder — no mutex inside. After construction, the caller hands it off to `into_scraper() -> MetricsScraper`, a cheap clone-able handle the HTTP server task uses (carries `Arc<Registry>`).

## Slot opaqueness

`Summary` and `TimeIntegratingGauge` both wrap `Arc<Inner>` internally. Callers see only the bare wrapper type — `Clone` is cheap (Arc bump), `Collector` is implemented on the wrapper directly so registration goes through `registry.register(Box::new(summary.clone()))`. No `CollectorArc` newtype, no leaked `Arc<Slot>` from `allocate()`.

Slots are stored as `Mutex<Vec<SlotInner>>` on the parent — one lock, no per-slot mutex. Handles are `pub struct SummarySlot { parent: Summary, index: usize }`. `observe()` locks the parent's slot vec, indexes into the inner, mutates.

## Custom collectors

`prometheus` 0.14.0 ships `Counter`, `Gauge`, `IntCounter`, `IntGauge`, `Histogram`, `Registry`, and the `Collector` trait. It does **not** ship a Summary. The project's `Summary` and `TimeIntegratingGauge` are user implementations.

`Summary` quantiles use `sketches-ddsketch` 0.4.0 (1% relative-error guarantee, in-place `merge`). The sliding window is a `VecDeque<Bucket>` of DDSketches at fixed `bucket_granularity` (default 1s); eviction is lazy on every `record` / `merge_live`. On scrape every live bucket is merged into a fresh sketch and the configured quantiles read out. **Cumulative `_count` / `_sum`** are tracked on the slot itself (not per-bucket) so they follow the Prometheus Summary contract — monotonic, never decrease on bucket eviction.

`TimeIntegratingGauge` uses Kahan summation on `acc + last_value * dt`. Suffix the metric name with `_seconds_integral` — enforced at construction by `debug_assert!`. Read time-averaged values via `rate(metric[window])`.

## Lock vs pool: three distinct concerns

The validator-flagged design split:

**a. Lock stats (semaphore-level, per-flow concurrency)** — `LockRecorder` from `air_elt_monitoring::LockRecorder`. Used by `FlowLockHandle` in `core::util::concurrency`. Time-integrating only:
- `air_elt_lock_queue_seconds_integral{kind, component}` — time-integral of callers waiting on the lock.
- `air_elt_lock_active_seconds_integral{kind, component}` — time-integral of held permits.

Plain "current value" gauges are deliberately not emitted; an operator who wants a snapshot reads the integral at two adjacent scrapes (or differentiates via `rate(...)`).

**b. Lock max (configuration; one-shot)** — `MonitoringManager::set_lock_max(kind, name, max)` directly. Emits `air_elt_lock_max{kind, component}`. Called from `assemble` once per `[[sources]]`/`[[sinks]]`/`[[storages]]`. Outside any concurrency code.

**c. Connector pool stats (driver-level, event-driven)** — `MonitoringManager::pool_stats_recorder(kind, name, max, min) -> PoolStatsRecorder`. Minted by each `*Factory::build(cfg, monitoring)` after parsing `(max-connections, min-connections)` via `air_elt_commons::pool_settings::PoolSettings::resolve_bounds`, then threaded into the connector's `connect()`. The factory owns this end-to-end — there are no free helpers in `validation::pipeline` and no `pool_stats()` method on the `Source`/`Sink`/`Storage` traits.

Each backend ships a `PoolStatsReader` that the factory mints next to its pool and registers with monitoring:
- **sqlx (pg/mysql/questdb)** — `PgPoolStatsReader` / `MySqlPoolStatsReader` / `QuestDbPoolStatsReader` peek at sqlx internals (`pool.size()` - `pool.num_idle()` → `active`, `pool.num_idle()` → `idle`) on every `read()`. Cheap, no extra state. Wired by `PgSourceFactory` / `PgSinkFactory` / `PgStorageFactory` and the mysql/questdb equivalents.
- **mongodb** — `MongoPoolStatsReader` owns two `AtomicU32`s that the CMAP event handler updates inline: `ConnectionReady → on_pool_filled`; `ConnectionCheckedOut → on_idle_acquired`; `ConnectionCheckedIn → on_released_to_idle`; `ConnectionClosed{Idle/Stale} → on_closed_from_idle`; `ConnectionClosed{Error/Dropped} → on_closed_from_active`. `PoolClosed` is deliberately a no-op (see the note at the end of this section). Wired in `air_elt_commons_mongodb::client::connect`.
- **ClickHouse (HTTP/reqwest)** — no driver-pool concept; its factory accepts the `monitoring` handle and ignores it. No reader is minted, so `air_elt_pool_connections_*` carries no rows for ClickHouse components.

Active and idle are plain `IntGaugeVec` children. The collector pulls live counts from a `PoolStatsReader` (one per backend; trait method `read() -> PoolConnectionCounts`) on every scrape and writes them through the recorder:
- `air_elt_pool_connections_active{kind, component}` (plain gauge, instant value at last scrape)
- `air_elt_pool_connections_idle{kind, component}` (plain gauge, instant value at last scrape)
- `air_elt_pool_connections_max{kind, component}` (plain gauge, baked in at recorder mint)
- `air_elt_pool_connections_min{kind, component}` (plain gauge, same)

Time-integrating gauges for active/idle were intentionally dropped — at scrape cadence (~10s) they only integrate the last sampled value constantly between scrapes, giving the same resolution as the plain gauge plus a one-scrape lag. Re-add only if an event-driven path emerges (mongo CMAP atomics already collect that info; sqlx would need its own).

sqlx readers peek at the driver's authoritative counters on every `read()`, so the active/idle gauges always reflect the pool state at scrape time — no callback-coverage drifts. Mongo's CMAP atomics cover every transition (`ConnectionReady`, `CheckedOut`, `CheckedIn`, `ConnectionClosed{Idle/Stale/Error/Dropped}`); we deliberately ignore `ConnectionClosed{PoolClosed}` because it sweeps both idle and active conns at shutdown, which would drive a counter negative.

## Per-stage error tagging

`RuntimeError::kind() -> &'static str` (in `core::error`) is the single source of truth for the `kind` label. The runner tags `stage` at each I/O call site via `.inspect_err(|e| self.metrics.record_error(ErrorStage::X, e.kind()))?` — no heuristic classifier. Stages: `Fetch`, `Transform`, `Sink`, `Storage`, `Other`.

`errors_total{flow, stage, stage_kind, stage_name, kind}` materialises children lazily on first observation (no pre-extraction).

## Disabled mode

`PrometheusConfig::enabled = false` (default) makes `MonitoringManager::new` return the no-op variant. No HTTP server is started. `flow_recorder` / `lock_recorder` / `pool_stats_recorder` return `disabled()` recorders that early-return on every method. Cost of an instrumented call site is one enum-discriminant load.

## Config

`[metrics.prometheus]`. Every field has a default; `enabled = true` alone produces a working setup (port 8080, prefix `/metrics`, 5s summary window, 1s bucket granularity, quantiles `[0.5, 0.9, 0.99]`). Validation in `air_elt_monitoring::config::PrometheusConfig::validate`, invoked from `core::config::loader::validate_post_merge`. The TOML/YAML duration parser comes from `air-elt-commons::interval` — no parser duplication.

## Where current metrics live

- **`FlowRecorder`** (`crates/monitoring/src/recorders/flow_recorder.rs`): `air_elt_fetch_seconds` / `transform` / `sink` (Summary, per-flow + `_global`); `air_elt_rows_total{flow, stage, component, component_kind, op}` (IntCounterVec — one family folds read/written/skipped via `stage`; `component`/`component_kind` are the source on `stage=read` and the sink on `stage=written|skipped`); `air_elt_errors_total{flow, stage, stage_kind, stage_name, kind}` (IntCounterVec, lazy).
- **`LockRecorder`** (`crates/monitoring/src/recorders/lock_recorder.rs`): time-integrating gauges only (see "Lock vs pool" above). Lifetime-borrowed `QueueGuard<'a>` / `ActiveGuard<'a>` (no Arc clone per guard).
- **`PoolStatsCollector`** + **`PoolStatsRecorder`** + **`PoolStatsReader`** (`crates/monitoring/src/recorders/pool_stats_collector.rs`): snapshot-at-scrape driver-pool accounting. The collector owns four `IntGaugeVec` families (active/idle/max/min); each `register_recorder(kind, name, max, min)` pins the max/min children and returns a `PoolStatsRecorder`. The factory then mints a per-backend `PoolStatsReader` (`PgPoolStatsReader`, `MySqlPoolStatsReader`, `QuestDbPoolStatsReader`, `MongoPoolStatsReader`) and registers it via `MonitoringManager::register_pool_stats_reader`; on every scrape the collector calls `reader.read()` and writes the result into the recorder's active/idle gauges. The reader stores a `Weak`, so when the connector drops, the labelled rows are evicted automatically.
- **`ProcessCollector`** (`crates/monitoring/src/recorders/process_collector.rs`): `process_cpu_seconds_total` (IntCounter, delta-driven from `sysinfo::Process::accumulated_cpu_time()`); `process_resident_memory_bytes`, `process_start_time_seconds`, `cpu_count`, `memory_total_bytes` (plain gauges); `memory_{used,available,free}_bytes_seconds_integral` (time-integrating). Driven by `sysinfo` 0.37.2; refresh happens inside `Collector::collect`.
- **`CountsCollector`**: plain `IntGauge`s for `flows` / `sources` / `sinks` / `storages` (no suffix — these are cardinality gauges, not counters and not the Summary `_count` pair), set once at app start by `MonitoringManager::set_counts`.

## HTTP server lifecycle

`App::spawn_metrics(rx) -> Option<JoinHandle<()>>` consumes the manager via `into_scraper()`, spawns the axum task with `with_graceful_shutdown(watch_changed)`. `main.rs` `await`s the join handle after the watch flips — no `abort()` mid-flight. Returns `None` when monitoring is disabled.

The manager is wrapped in `parking_lot::Mutex<Option<MonitoringManager>>` on `App` for interior mutability under `&self`. `flows_assembled` borrows it through a RAII `MonitoringGuard` that restores the inner on drop (panic / cancellation safe). `spawn_metrics` then `.take()`s the manager out, leaving the slot `None` post-spawn.

## Naming

- All metric names prefixed `air_elt_` except the platform-standard `process_*`, `memory_*`, `cpu_count`, `flows` / `sources` / `sinks` / `storages`.
- Suffixes: `_total` for monotonically-increasing counters, `_seconds` for durations (Summary), `_seconds_integral` for time-integrating counters, `_bytes` for byte counts.
- Low cardinality only. `flow`, `stage`, `stage_kind`, `stage_name`, `kind`, `op`, `component` are all bounded enums or operator-declared identifiers. No request IDs, no user IDs, no row keys.
