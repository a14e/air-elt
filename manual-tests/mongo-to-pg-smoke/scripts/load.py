# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "pymongo==4.10.1",
#   "motor==3.7.0",
# ]
# ///
"""Continuous load simulator for the manual mongo → postgres smoke test.

One persistent motor client, one generator coroutine. No worker pools, no
queue — the workload is small enough that a single coroutine driving
``bulk_write(ordered=False)`` is the right shape (the mongo wire
protocol's batching is the equivalent of pg's pipeline mode).

Mixed traffic:
* 80% of operations insert a fresh ``seq`` (NEW path).
* 20% replay a recent ``_id`` via ``replace_one(upsert=True)`` — air-elt's
  cursor on ``inserted_at`` re-emits the row downstream, exercising the
  conflict-resolution path on the postgres sink.

Per-tick jitter (±50%) desynchronises the load from any other periodic
work (e.g. mongo replica-set heartbeats) and presents a more realistic
arrival pattern than a fixed cadence.

Exits cleanly on SIGINT / SIGTERM (Ctrl-C) with a summary, including the
final ``deficit_ops`` count if the loop fell behind the target rate.
"""

from __future__ import annotations

import argparse
import asyncio
import datetime as dt
import itertools
import os
import random
import signal
import sys
import time
from collections import deque
from typing import Any

from bson.decimal128 import Decimal128
from motor.motor_asyncio import AsyncIOMotorClient, AsyncIOMotorCollection
from pymongo import InsertOne, ReplaceOne
from pymongo.errors import BulkWriteError

DEFAULT_URL = "mongodb://localhost:27117/appdb"

# Cap on the recent-id deque used to pick REPLAY targets.
RECENT_IDS_CAP = 200


def _build_doc(seq: int, now: dt.datetime) -> dict[str, Any]:
    return {
        "seq": seq,
        "name": f"user-{seq}",
        "email": f"user-{seq}@example.com",
        "is_active": bool(seq % 2),
        "age": 18 + (seq % 60),
        "score": round(random.uniform(0.0, 100.0), 4),
        "balance": Decimal128(f"{(seq * 7.13) % 10000:.2f}"),
        "tags": ["alpha", "beta"] if seq % 3 else ["gamma"],
        "inserted_at": now,
    }


def _install_signal_handlers(stop_event: asyncio.Event) -> None:
    if sys.platform != "win32":
        loop = asyncio.get_running_loop()
        for sig in (signal.SIGINT, signal.SIGTERM):
            loop.add_signal_handler(sig, stop_event.set)
    else:
        # add_signal_handler raises NotImplementedError on the Windows
        # ProactorEventLoop; fall back to the synchronous handler.
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, lambda *_: stop_event.set())


async def _ensure_seq_index(coll: AsyncIOMotorCollection) -> None:
    """``find_one(sort=[(seq, -1)])`` at startup needs an index to be fast.
    Idempotent: ``create_index`` is a no-op if the index already exists."""
    await coll.create_index("seq")


async def generator(
    coll: AsyncIOMotorCollection,
    rate: float,
    batch_size: int,
    duration: float,
    update_pct: int,
    start_seq: int,
    counter: list[int],
    deficit_ops: list[int],
    stop_event: asyncio.Event,
) -> int:
    next_seq = itertools.count(start_seq)
    recent_ids: deque[Any] = deque(maxlen=RECENT_IDS_CAP)
    target_interval = batch_size / rate
    last_tick = time.monotonic()
    expected_committed = 0.0
    deadline = time.monotonic() + duration
    total_committed = 0

    while not stop_event.is_set() and time.monotonic() < deadline:
        now_dt = dt.datetime.now(tz=dt.timezone.utc)
        ops: list[Any] = []
        for _ in range(batch_size):
            if recent_ids and random.randint(1, 100) <= update_pct:
                target_id = random.choice(recent_ids)
                seq = next(next_seq)
                doc = _build_doc(seq, now_dt)
                # `replace_one` with `upsert=True` reuses the existing _id.
                # The cursor bumps because `inserted_at` is updated.
                ops.append(ReplaceOne({"_id": target_id}, doc, upsert=True))
            else:
                seq = next(next_seq)
                ops.append(InsertOne(_build_doc(seq, now_dt)))

        try:
            result = await coll.bulk_write(ops, ordered=False)
            # Capture inserted ids for the REPLAY pool; replace results don't
            # surface ids, so we just track new inserts.
            for inserted_id in (result.inserted_ids or {}).values():
                recent_ids.append(inserted_id)
            committed = len(ops)
            total_committed += committed
            counter[0] += committed
        except BulkWriteError as exc:
            # Partial failures still report write_count; log them.
            committed = exc.details.get("nInserted", 0) + exc.details.get("nModified", 0)
            total_committed += committed
            counter[0] += committed
            print(
                f"[load.py] bulk_write partial failure: nInserted="
                f"{exc.details.get('nInserted', 0)} nModified="
                f"{exc.details.get('nModified', 0)} writeErrors="
                f"{len(exc.details.get('writeErrors', []))}",
                file=sys.stderr,
                flush=True,
            )
        except Exception as exc:
            print(f"[load.py] bulk_write error: {exc!r}", file=sys.stderr, flush=True)

        # Deficit tracking — keep the gap visible.
        now = time.monotonic()
        elapsed = now - last_tick
        last_tick = now
        expected_committed += rate * elapsed
        gap = int(expected_committed - counter[0])
        if gap > batch_size * 4:
            deficit_ops[0] = gap

        # Jittered sleep.
        sleep_for = target_interval * random.uniform(0.5, 1.5)
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=sleep_for)
            break
        except asyncio.TimeoutError:
            pass

    return total_committed


async def _reporter(
    counter: list[int],
    deficit_ops: list[int],
    deadline: float,
    stop_event: asyncio.Event,
) -> None:
    last_count = 0
    last_at = time.monotonic()
    while not stop_event.is_set() and time.monotonic() < deadline:
        try:
            await asyncio.wait_for(stop_event.wait(), timeout=2.0)
            break
        except asyncio.TimeoutError:
            pass
        now = time.monotonic()
        cur = counter[0]
        window = now - last_at
        rate_observed = (cur - last_count) / window if window > 0 else 0.0
        print(
            f"[load.py] total_committed={cur} observed_rate={rate_observed:.1f}/s "
            f"deficit_ops={deficit_ops[0]}",
            flush=True,
        )
        last_count = cur
        last_at = now


async def run(args: argparse.Namespace) -> int:
    stop_event = asyncio.Event()
    _install_signal_handlers(stop_event)

    client = AsyncIOMotorClient(args.url)
    try:
        coll = client.get_database("appdb").get_collection("users")
        await _ensure_seq_index(coll)
        last = await coll.find_one(sort=[("seq", -1)])
        start_seq = int(last["seq"]) + 1 if last and "seq" in last else 1

        print(
            f"[load.py] url={args.url} rate={args.rate}/s batch_size={args.batch_size} "
            f"update_pct={args.update_pct}% duration={args.duration}s "
            f"starting_seq={start_seq}",
            flush=True,
        )

        counter = [0]
        deficit_ops = [0]
        deadline = time.monotonic() + args.duration

        reporter_task = asyncio.create_task(
            _reporter(counter, deficit_ops, deadline, stop_event)
        )
        total = await generator(
            coll,
            args.rate,
            args.batch_size,
            args.duration,
            args.update_pct,
            start_seq,
            counter,
            deficit_ops,
            stop_event,
        )
        stop_event.set()
        await reporter_task

        reason = "signal" if time.monotonic() < deadline else "duration"
        print(
            f"[load.py] DONE total_committed={total} deficit_ops={deficit_ops[0]} "
            f"reason={reason}",
            flush=True,
        )
        return 0
    finally:
        client.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", default=os.environ.get("MONGO_URL", DEFAULT_URL))
    parser.add_argument(
        "--rate",
        type=float,
        default=20.0,
        help="Target ops/s (default 20). Smoke test, not high-rate.",
    )
    parser.add_argument(
        "--duration",
        type=int,
        default=360,
        help="Total run time in seconds (default 360 — 6 minutes).",
    )
    parser.add_argument(
        "--batch-size",
        type=int,
        default=50,
        help=(
            "Docs per bulk_write call (default 50). Each tick assembles "
            "this many operations and ships them via bulk_write(ordered=False), "
            "amortising mongo wire-protocol round-trips."
        ),
    )
    parser.add_argument(
        "--update-pct",
        type=int,
        default=20,
        help=(
            "Share of ops that replay a recent _id via replace_one(upsert=True) "
            "(default 20). The remainder are fresh inserts."
        ),
    )
    args = parser.parse_args()
    if args.batch_size < 1:
        print("[load.py] --batch-size must be >= 1", file=sys.stderr)
        return 2
    if not 0 <= args.update_pct <= 100:
        print("[load.py] --update-pct must be in [0, 100]", file=sys.stderr)
        return 2
    return asyncio.run(run(args))


if __name__ == "__main__":
    sys.exit(main())
