# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "psycopg[binary]==3.2.4",
#   "PyYAML==6.0.2",
# ]
# ///
"""Async load generator for the thousand-flows-test scaffold.

One persistent pipelined ``psycopg.AsyncConnection`` per source pg (10 at
default topology). Each source has its own coroutine — N source generators
run concurrently under ``asyncio.gather``. No pools, no producer/consumer
queue, no semaphores: pipeline mode means one connection routinely sustains
tens of thousands of inserts per second.

Each generator owns its source's tables exclusively, so per-table ids are
client-generated and monotonic. 80% of rows use a fresh id (NEW path);
``update_pct`` of rows replay an existing id from a recent-ids deque — that
re-fires the row through ``ON CONFLICT (id) DO UPDATE`` and re-emits it
downstream via the cursor's ``updated_at`` index.

Pacing uses per-table debt accumulators so fractional rps (0.5/s) emit
correctly across many ticks, plus ±50% per-tick jitter so generators
desynchronise across sources and don't all hit pg in waves.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import random
import signal
import sys
import time
from collections import deque
from decimal import Decimal
from pathlib import Path
from typing import Any

import psycopg
import yaml

HERE = Path(__file__).resolve().parent
TEST_ROOT = HERE.parent
TOPOLOGY_PATH = TEST_ROOT / "topology.yaml"

CURRENCIES = ["USD", "EUR", "GBP", "JPY", "BRL"]
STATUSES = ["created", "paid", "shipped", "refunded", "cancelled"]
DESCRIPTIONS = [
    "first order",
    "rush delivery",
    "promo applied",
    "gift wrap",
    "subscription renewal",
    "manual override",
    "audit retry",
    "refund pending",
    "partial fulfilment",
    "loyalty bonus",
]

# Cap on the per-table recent-id ring used to pick REPLAY targets. Larger
# = more varied replay distribution; smaller = less memory. 200 keeps the
# total footprint at ~16 MB across 10k tables.
RECENT_IDS_CAP = 200


# Single template — both NEW and REPLAY paths share it because the worker
# generates ids client-side. NEW rows have an unused id (no row yet → INSERT
# path runs), REPLAY rows have a known id (row exists → DO UPDATE path runs).
UPSERT_SQL_TEMPLATE = (
    "INSERT INTO public.t_{nnn} "
    "(id, user_id, email, amount, currency, status, description, "
    "quantity, is_active, metadata) "
    "VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s) "
    "ON CONFLICT (id) DO UPDATE SET "
    "amount = EXCLUDED.amount, status = EXCLUDED.status, "
    "description = EXCLUDED.description, quantity = EXCLUDED.quantity, "
    "updated_at = NOW()"
)


def _upsert_sql_for(tbl_idx: int) -> str:
    return UPSERT_SQL_TEMPLATE.format(nnn=f"{tbl_idx:03d}")


def _make_params(src_idx: int, row_id: int) -> tuple[Any, ...]:
    user_id = random.randint(1, 100_000)
    email = f"user-{user_id}@example.com"
    amount = Decimal(f"{random.uniform(0.01, 10000.00):.2f}")
    currency = random.choice(CURRENCIES)
    status = random.choice(STATUSES)
    description = random.choice(DESCRIPTIONS) if random.random() < 0.7 else None
    quantity = random.randint(1, 50)
    is_active = bool(random.getrandbits(1))
    if random.random() < 0.5:
        metadata = json.dumps(
            {"src": f"src_pg_{src_idx:02d}", "tags": ["alpha", "beta"]},
            separators=(",", ":"),
        )
    else:
        metadata = None
    return (
        row_id,
        user_id,
        email,
        amount,
        currency,
        status,
        description,
        quantity,
        is_active,
        metadata,
    )


class GeneratorStats:
    """Shared, per-source counters drained by the reporter coroutine."""

    def __init__(self, src_count: int) -> None:
        self.committed: list[int] = [0] * src_count
        self.errors: list[int] = [0] * src_count
        self.deficit_ops: list[int] = [0] * src_count


async def source_generator(
    src_idx: int,
    url: str,
    tables: list[int],
    rps_per_table: dict[int, float],
    batch_size: int,
    update_pct: int,
    deadline: float,
    stop_event: asyncio.Event,
    stats: GeneratorStats,
) -> None:
    """Drive one source pg with one pipelined connection, reopening on error.

    The outer loop owns the connection lifecycle: if any error surfaces
    from the inner tick loop (closed connection, server-side crash
    recovery, network blip), close the bad connection and reopen a fresh
    one after a short backoff. Under our previous "one persistent
    connection forever" design, a single backend SIGPIPE on the pg side
    triggered postmaster crash recovery (terminating ALL backends on
    that source pg), which left our connection permanently closed and
    spammed `OperationalError('the connection is closed')` every tick
    until SIGTERM. The reconnect loop keeps the source alive across
    those events.

    Inner tick: walk the table set, top up each table's debt by
    ``rps * elapsed``, emit ``int(debt)`` rows per table through one
    pipeline cycle, then sleep ``target_interval × U(0.5, 1.5)``. Tick
    rate self-adjusts: if pg is slow, ``elapsed`` grows, debt
    accumulates, the next tick fires more rows — ``deficit_ops``
    reporting makes underdelivery visible.

    Per-table state (``next_id``, ``recent_ids``, ``debt``) survives
    reconnects so the workload continues from where it left off rather
    than restarting at id=1 (which would mass-collide with the existing
    rows and stress the conflict path artificially).
    """
    next_id: dict[int, int] = {t: 1 for t in tables}
    recent_ids: dict[int, deque[int]] = {
        t: deque(maxlen=RECENT_IDS_CAP) for t in tables
    }
    # Random initial debt offset so generators don't synchronise on first tick.
    debt: dict[int, float] = {
        t: random.random() * rps_per_table[t] * 0.1 for t in tables
    }
    upsert_sql: dict[int, str] = {t: _upsert_sql_for(t) for t in tables}

    total_rps = sum(rps_per_table.values())
    if total_rps <= 0:
        return
    target_interval = batch_size / total_rps

    expected_committed = 0.0
    last_tick = time.monotonic()
    reconnect_backoff = 0.5

    # Outer reconnect supervisor.
    while not stop_event.is_set() and time.monotonic() < deadline:
        conn: psycopg.AsyncConnection | None = None
        try:
            conn = await psycopg.AsyncConnection.connect(url, autocommit=True)
            reconnect_backoff = 0.5  # reset on successful connect
            # Inner tick loop — runs until error, deadline, or stop_event.
            while not stop_event.is_set() and time.monotonic() < deadline:
                now = time.monotonic()
                elapsed = now - last_tick
                last_tick = now

                # Accumulate debt; build batch.
                batch: list[tuple[str, tuple[Any, ...]]] = []
                for t in tables:
                    debt[t] += rps_per_table[t] * elapsed
                    n = int(debt[t])
                    if n <= 0:
                        continue
                    debt[t] -= n
                    for _ in range(n):
                        if recent_ids[t] and random.randint(1, 100) <= update_pct:
                            row_id = random.choice(recent_ids[t])
                        else:
                            row_id = next_id[t]
                            next_id[t] += 1
                            recent_ids[t].append(row_id)
                        batch.append((upsert_sql[t], _make_params(src_idx, row_id)))

                if batch:
                    async with conn.pipeline():
                        cur = conn.cursor()
                        for sql, params in batch:
                            await cur.execute(sql, params)
                    stats.committed[src_idx] += len(batch)

                # Deficit tracking — compare actual committed against expected
                # at this wall-clock point. Positive deficit = can't keep up.
                expected_committed += total_rps * elapsed
                actual = stats.committed[src_idx]
                deficit = int(expected_committed - actual)
                if deficit > batch_size * 4:
                    stats.deficit_ops[src_idx] = deficit

                # Jittered sleep.
                sleep_for = target_interval * random.uniform(0.5, 1.5)
                try:
                    await asyncio.wait_for(stop_event.wait(), timeout=sleep_for)
                    break
                except asyncio.TimeoutError:
                    pass
        except Exception as exc:
            stats.errors[src_idx] += 1
            print(
                f"[load.py] src_pg_{src_idx:02d} connection error "
                f"(will reconnect in {reconnect_backoff:.1f}s): {exc!r}",
                file=sys.stderr,
                flush=True,
            )
            # Backoff before reconnecting. Cap so we keep trying.
            try:
                await asyncio.wait_for(stop_event.wait(), timeout=reconnect_backoff)
                break
            except asyncio.TimeoutError:
                pass
            reconnect_backoff = min(reconnect_backoff * 2.0, 10.0)
        finally:
            if conn is not None:
                try:
                    await conn.close()
                except Exception:
                    pass

        # Jittered sleep.
        sleep_for = target_interval * random.uniform(0.5, 1.5)
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=sleep_for)
            break
        except asyncio.TimeoutError:
            pass


async def reporter(
    stats: GeneratorStats,
    src_count: int,
    deadline: float,
    stop_event: asyncio.Event,
) -> None:
    last_total = 0
    last_at = time.monotonic()
    while not stop_event.is_set() and time.monotonic() < deadline:
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=5.0)
            break
        except asyncio.TimeoutError:
            pass
        now = time.monotonic()
        total = sum(stats.committed)
        errors = sum(stats.errors)
        deficit = sum(stats.deficit_ops)
        window = now - last_at
        rate = (total - last_total) / window if window > 0 else 0.0
        print(
            f"[load.py] total_committed={total} observed_rate={rate:.1f}/s "
            f"errors={errors} deficit_ops={deficit}",
            flush=True,
        )
        last_total = total
        last_at = now


def load_topology() -> dict[str, Any]:
    return yaml.safe_load(TOPOLOGY_PATH.read_text(encoding="utf-8"))


def _install_signal_handlers(stop_event: asyncio.Event) -> None:
    if sys.platform != "win32":
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, stop_event.set)
    else:
        # add_signal_handler raises NotImplementedError on the Windows
        # ProactorEventLoop; fall back to the synchronous signal handler.
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, lambda *_: stop_event.set())


async def run(args: argparse.Namespace) -> int:
    stop_event = asyncio.Event()
    _install_signal_handlers(stop_event)

    top = load_topology()
    src_count = int(top["sources"]["count"])
    tables_per_source = int(top["sources"]["tables_per_source"])
    heavy_per_source = int(top["sources"]["heavy_tables_per_source"])
    src_pg_base = int(top.get("ports", {}).get("source_pg_base", 55100))
    load_cfg = top.get("load", {}) or {}
    heavy_rps = float(load_cfg.get("heavy_rps", 100.0))
    light_rps = float(load_cfg.get("light_rps", 0.5))
    batch_size = int(load_cfg.get("batch_size", 100))
    duration = args.duration if args.duration > 0 else int(load_cfg.get("duration_secs", 180))
    update_pct = int(load_cfg.get("update_pct", 20))

    if args.profile == "heavy-only":
        heavy_per_source = tables_per_source
        light_rps = 0.0

    tables = list(range(tables_per_source))
    rps_per_table = {
        t: heavy_rps if t < heavy_per_source else light_rps for t in tables
    }
    total_target_per_source = sum(rps_per_table.values())
    aggregate_target = src_count * total_target_per_source
    print(
        f"[load.py] target ~{aggregate_target:.1f} ops/s aggregate "
        f"({src_count} sources × {total_target_per_source:.1f}/s each, "
        f"heavy={heavy_per_source}@{heavy_rps}/s, "
        f"light={tables_per_source - heavy_per_source}@{light_rps}/s), "
        f"batch_size={batch_size}, duration={duration}s",
        flush=True,
    )

    # Resolve URLs once; each source_generator owns its connection lifecycle
    # internally (open / detect failure / reconnect with backoff).
    urls: list[str] = []
    for i in range(src_count):
        url = os.environ.get(f"SRC_PG_{i:02d}_URL")
        if not url:
            sys.exit(
                f"[load.py] SRC_PG_{i:02d}_URL not set; "
                "did run.py source .env.generated?"
            )
        urls.append(url)
        print(
            f"[load.py] source url src_pg_{i:02d} (port {src_pg_base + i}) resolved",
            flush=True,
        )

    stats = GeneratorStats(src_count)
    deadline = time.monotonic() + duration

    reporter_task = asyncio.create_task(
        reporter(stats, src_count, deadline, stop_event)
    )
    await asyncio.gather(
        *[
            source_generator(
                i,
                urls[i],
                tables,
                rps_per_table,
                batch_size,
                update_pct,
                deadline,
                stop_event,
                stats,
            )
            for i in range(src_count)
        ],
        return_exceptions=False,
    )
    stop_event.set()
    await reporter_task

    total = sum(stats.committed)
    errors = sum(stats.errors)
    deficit = sum(stats.deficit_ops)
    reason = "signal" if time.monotonic() < deadline else "duration"
    print(
        f"[load.py] DONE total_committed={total} errors={errors} "
        f"deficit_ops={deficit} reason={reason}",
        flush=True,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--duration",
        type=int,
        default=0,
        help="Override topology.yaml.load.duration_secs (0 = use topology value).",
    )
    parser.add_argument(
        "--profile",
        choices=("default", "heavy-only"),
        default="default",
        help="`heavy-only` promotes every table to heavy_rps for stress testing.",
    )
    args = parser.parse_args()
    return asyncio.run(run(args))


if __name__ == "__main__":
    sys.exit(main())
