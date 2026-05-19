# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "pymongo==4.10.1",
#   "psycopg[binary]==3.2.4",
# ]
# ///
"""Intermediate validator for the manual mongo → postgres smoke test.

Periodically polls both databases and prints one line per tick: total
counts, the current signed row lag (mongo - pg), and the max ``seq``
already in Postgres. Mongo count is exact (``count_documents({})``) so
the lag against pg's exact ``count(*)`` is commensurable.
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import signal
import sys
import time
from typing import Any

import psycopg
from pymongo import MongoClient

DEFAULT_MONGO_URL = "mongodb://localhost:27117/appdb"
DEFAULT_PG_URL = "postgres://air:air@localhost:54322/airdata"
_STOP = False


def _on_signal(_signum: int, _frame: Any) -> None:
    global _STOP
    _STOP = True


def _drift_free_sleep(interval: float) -> None:
    end = time.monotonic() + interval
    while not _STOP:
        remaining = end - time.monotonic()
        if remaining <= 0:
            return
        time.sleep(min(0.2, remaining))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--mongo-url", default=os.environ.get("MONGO_URL", DEFAULT_MONGO_URL)
    )
    parser.add_argument(
        "--pg-url", default=os.environ.get("PG_SINK_URL", DEFAULT_PG_URL)
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=5.0,
        help="Seconds between polls (default 5).",
    )
    args = parser.parse_args()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    mongo: MongoClient[dict[str, Any]] = MongoClient(args.mongo_url)
    pg: psycopg.Connection | None = None

    def _query_pg() -> tuple[int, int]:
        """Run the count + max(seq) query, reopening pg on connection drop.

        Mirrors the thousand-flows-test PgChannel pattern: if a pg
        backend crash (SIGPIPE → postmaster crash recovery) closes our
        persistent socket, we reopen it instead of staying broken for
        the rest of the run.
        """
        nonlocal pg
        for attempt in (1, 2):
            try:
                if pg is None:
                    pg = psycopg.connect(args.pg_url, autocommit=True)
                with pg.cursor() as cur:
                    cur.execute(
                        "SELECT count(*), COALESCE(max(seq), 0) FROM public.users"
                    )
                    row = cur.fetchone()
                    assert row is not None
                    return int(row[0]), int(row[1])
            except (psycopg.OperationalError, psycopg.InterfaceError) as exc:
                if attempt == 1:
                    print(
                        f"[validate.py] pg connection dropped, reconnecting: {exc!r}",
                        file=sys.stderr,
                        flush=True,
                    )
                    try:
                        if pg is not None:
                            pg.close()
                    except Exception:
                        pass
                    pg = None
                    continue
                raise
        raise RuntimeError("unreachable")

    try:
        coll = mongo.get_database("appdb").get_collection("users")
        print(
            f"[validate.py] mongo={args.mongo_url} pg={args.pg_url} "
            f"interval={args.interval}s",
            flush=True,
        )

        ticks = 0
        last_pg = 0
        while not _STOP:
            # `count_documents({})` is exact at the cost of a brief
            # collection scan. At smoke-test rates (20 ops/s for ~6 min)
            # the cost is negligible and the number is commensurable with
            # pg's exact `count(*)`.
            mongo_total = coll.count_documents({})
            try:
                pg_total, pg_max_seq = _query_pg()
            except Exception as exc:
                print(
                    f"[validate.py] ERR pg: {exc!r}",
                    file=sys.stderr,
                    flush=True,
                )
                ticks += 1
                _drift_free_sleep(args.interval)
                continue
            delta = pg_total - last_pg
            last_pg = pg_total
            # Signed lag — negative means pg is somehow AHEAD of mongo
            # (only possible during teardown reordering or a stale read).
            # Clamping to 0 would hide that.
            lag = mongo_total - pg_total
            stamp = dt.datetime.now(tz=dt.timezone.utc).strftime("%H:%M:%S")
            print(
                f"[validate.py] {stamp} mongo={mongo_total} pg={pg_total} "
                f"lag_rows={lag} pg_max_seq={pg_max_seq} delta_pg=+{delta}",
                flush=True,
            )
            ticks += 1
            _drift_free_sleep(args.interval)

        print(f"[validate.py] DONE ticks={ticks}", flush=True)
        return 0
    finally:
        if pg is not None:
            try:
                pg.close()
            except Exception:
                pass
        try:
            mongo.close()
        except Exception:
            pass


if __name__ == "__main__":
    sys.exit(main())
