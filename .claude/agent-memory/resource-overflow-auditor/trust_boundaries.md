---
name: Air Elt trust boundaries
description: Where external input enters the Air Elt ELT service and what bounds are / are not applied. Revisit before future resource-safety audits.
type: reference
---

Trust boundaries in the Air Elt ELT service (as of 2026-04 / branch AIR-1):

- **TOML config files** (`crates/core/src/config/loader.rs`) — loaded via `std::fs::read_to_string` with a **16 MiB cap** (`MAX_CONFIG_BYTES`). `[config].include` rejects absolute paths; symlink loops are deduped by canonical-path `HashSet`; directories read non-recursively for `*.toml` only.
- **`${VAR}` / `${VAR:default}` expansion** (`crates/core/src/config/env_expand.rs`) — single-pass `Regex::replace_all`. **Secrets are not themselves expanded** (explicit design: no recursion → no cycles → no depth guard needed). Lookup order: process env → `[secrets]` → default → error.
- **Postgres row contents** — flow through `PgSource::read_batch` → `Vec<CoreRow>` → `PgSink::write_batch`. Bounded by `batch_limit × mapping_cols ≤ 60_000` (enforced at load time because PG bind-count is a u16).
- **`AIR_ELT_TEST_PG_URL` + `DOCKER_HOST` env vars** — test-only path in `commons/testing/src/pg.rs`.

Applied bounds (2026-04):
- `batch_limit`: default 1024, must be ≥ 1, `batch_limit × cols ≤ 60_000` at `validate_post_merge`.
- `i64::try_from(spec.limit)` guards `LIMIT $N` bind (can't overflow in practice — usize≥i64 only on 128-bit platforms).
- Pool defaults (`PoolTimeouts::defaults`): connect 5s, acquire 10s, idle 300s, max_lifetime 1800s, statement 30s. `max_connections=5`. `connect_with` is also wrapped in `tokio::time::timeout` to guard even options-level hangs.
- Runner operation timeout wraps every `read_batch`/`write_batch`/`save_cursor`/`load_cursor` — default 30s, overridable per flow via `operation_timeout_secs`.
- `pg_pool()` test helper: `detect_backend` runs in `spawn_blocking` with a 300 ms tokio timeout. Stale `test_<ts>_*` schemas dropped if older than 24h at startup.

`unsafe` audit trail (as of 2026-04):
- `crates/commons/lib/src/secrets.rs:37` — test only, set_var, has `// Why:` comment.
- `crates/commons/testing/src/pg.rs:107` — `DOCKER_HOST` set_var, has `// Why:` comment. **NB:** this is in library code (not a test), but the crate is dev-only so it's acceptable. Only runs inside `pg_pool()` which is called exclusively from tests.
- `crates/core/src/config/env_expand.rs:70` — test only, set_var, has `// Why:` comment.
- `crates/core/src/config/loader.rs:316` — test only, set_var, has `// Why:` comment.

Known soft spots that could become bites under adversarial conditions:
- Daemon loop has **no backoff** on `write_batch` / `save_cursor` errors — `run_flow` returns `Err` at first failure, killing the flow. The rest of flows keep running (`run_all_flows` waits for all). There is no tight-loop risk because errors bail; the loop only stays in-bounds via `interval` sleep on empty drain and natural blocking on DB calls.
- `next_cursor is None` branch in `run_flow` (runner.rs:118) only warns and continues — if source repeatedly returns rows with `next_cursor=None`, the loop writes forever without cursor progress. Not currently possible with `PgSource` (always produces a next cursor when rows>0), but the contract doesn't enforce it.
- `stmt_ms = timeouts.statement.as_millis() as i64` in `pool.rs:81` — lossy cast from `u128` → `i64`, ok for any reasonable value but doesn't saturate; an operator-set timeout of > ~292M years overflows. Not worth fixing given the real-world envelope.

Crate layout map:
- `crates/app` — main, CLI, signal handling, task spawning for flows
- `crates/core` — config, validation, flow runner, traits, types
- `crates/commons/lib` — SQL quoting, pool, secret resolution (prod)
- `crates/commons/testing` — dev-dependency-only testcontainer + sandbox schema helper
- `crates/sources/postgres`, `crates/sinks/postgres`, `crates/storages/postgres` — connectors

All three pg connectors route through `air_elt_commons::sql::pg::pool::connect` — fixing pool settings in one place = fixing them in all three.
