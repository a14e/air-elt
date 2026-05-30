# Validation pipeline & concurrency (full reference)

Two stages: `assemble` (no I/O) → `validate` (probes, schema introspection, matrix, sampling). All assembled flows are driven concurrently through `futures::stream::iter(...).for_each_concurrent(None, validate_flow)` — no per-source grouping. Backend contention is bounded purely by the per-component `tokio::sync::Semaphore`s built in `assemble`, sized to each backend's `max-connections`. The CLI prints `running validation for {N} flows; semaphores cap {source=K, sink=L, storage=M}` at start. Output is sorted back into config order so error reporting is deterministic.

## Concurrency: per-component semaphores

`assemble` builds one `tokio::sync::Semaphore` per declared `[[sources]]`/`[[sinks]]`/`[[storages]]` instance, with permit count = the component's `max-connections` (capped at `Semaphore::MAX_PERMITS`). Flows sharing a component share the same `Arc<Semaphore>`. Each flow gets a `FlowLockHandle` (`core::util::concurrency`) that exposes `acquire_source()` / `acquire_sink()` / `acquire_storage()` — one permit per component kind.

**Locks must be strictly local — held only across the single I/O call that touches the component, then released. Never hold a permit across an unrelated `await` or across two backend calls; that's a parasitic block on sibling flows that share the pool.** The runner enforces this by scoping each `acquire_*` to a tight `{ let _g = ...acquire_X().await?; <single call> }` block: `ensure_built` takes source for source `build_context`, releases, takes sink for sink `build_context`, releases; cursor load/save take storage; `read_batch` / `sample` take source; `write_batch` takes sink. Transform runs without any permit (pure compute). The validation pipeline mirrors this — each probe / schema fetch scopes its own permit.

Because no call site ever holds two permits at once, **deadlock between flows is structurally impossible** — there is no canonical lock order to maintain, no AB-BA hazard to defend against. A long PG read in one flow no longer blocks an unrelated CH write in another flow that happens to share the storage; the two only contend on permits they both actually use.

Access probes inside validation are additionally wrapped in `retry_transient` (`core::util::retry`): three attempts (50 ms → 250 ms → 1.25 s), retrying only `RuntimeError::Backend`; every other error is authoritative and fails immediately. The runtime tick has its own exponential backoff (`1s → 4× → 1h cap`) on `Err`; the inter-tick idle sleep and the backoff sleep both happen AFTER the tick returns, so they never hold any permit. `dry_run` governs *what* the tick does (`sample` vs `read_batch`, no-op sink write, skip cursor save) — not *whether* it acquires.

`max-connections = 0` is rejected at `build_*` time (`PoolSettingsError::ZeroMaxConnections`): a zero-permit semaphore would hang every flow forever. Operators see a config error before any I/O is attempted.

## Validation toggles, schemaless, sampling

The flow-level `[flow.<name>.validation]` block exposes four toggles: `access`, `fields`, `inserts`, `sampling`. The first three default `true` and gate the access probes / matrix / sink write probe respectively. `sampling` follows the per-backend `SourceFactory::sampling_default()` — Mongo enables it (size 100), SQL keeps it disabled.

`Sink::schemaless()` is `true` for Mongo. The pipeline then derives the sink schema from the source's declared types, skipping the matrix narrowing check.

`Source::sample` is a single probe used by sampling-validation. The default delegates to `read_batch` with `spec.limit = n` and no cursor state — pull-based sources stay on the default so the probe exercises the same query the runner runs. CDC sources (`mongo-cdc`) override because their `read_batch` would block on the open change stream; the override aggregates `$sample` on the watched collection. Sampling-validation feeds the returned `RawBatch` through the compiled Transform.

`core::mapping::FieldPath` — `parse(&str)` produces a validated dot-notation path. SQL connectors reject `is_nested()` paths; Mongo accepts them.
