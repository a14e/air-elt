# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "psycopg[binary]==3.2.4",
#   "PyYAML==6.0.2",
# ]
# ///
"""Polling aggregator for the thousand-flows-test scaffold.

Emits one block per --interval:

    [validate.py] tick=N t=HH:MM:SS
      per_source     rows         max_id
      src_pg_00      ...
      ...
      per_sink       rows
      sink_pg_00     ...
      sink_ch_00     ...
      sink_qdb_00    ...
      TOTAL_SRC=...  TOTAL_SINK=...  max_lag=...

Counts:

* Source pg / sink pg row counts use ``pg_class.reltuples`` clamped with
  ``GREATEST(reltuples, 0)`` (fresh tables can report -1 before ANALYZE).
  These are *estimates* — exact ``count(*)`` across thousands of tables
  would dominate every tick.
* ``max(id)`` is exact and cheap on the indexed bigint pk.
* Sink-ch counts use ``count() FROM tbl FINAL`` for mutable
  ReplacingMergeTree tables (so pre-merge duplicates don't inflate the
  number) and ``sum(rows) FROM system.parts`` for append-only ones.
* All UNION-ALL queries are *chunked* (25 tables per statement) and run via
  a small thread pool, so SQL text size and per-statement memory stay
  bounded as topology grows.

Errors are loud — a failing backend reports ``ERR`` (not ``0``) and that
row is excluded from the ``max_lag`` calculation so a silent sink failure
can't masquerade as "sink caught up to source".
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import re
import signal
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

import psycopg
import yaml

HERE = Path(__file__).resolve().parent
TEST_ROOT = HERE.parent
TOPOLOGY_PATH = TEST_ROOT / "topology.yaml"

# Cap on tables per UNION ALL chunk. 25 keeps each SQL text under ~3 KB,
# which is comfortable for both pg's parser and CH's HTTP query length.
# Chunks run sequentially per backend (psycopg.Connection is not thread
# safe; CH's HTTP cost is amortised by chunking anyway), so a tick of 1000
# tables / source completes well within the default 5 s cadence.
CHUNK_SIZE = 25

_STOP = False


def _on_signal(_signum: int, _frame: Any) -> None:
    global _STOP
    _STOP = True


def load_topology() -> dict[str, Any]:
    return yaml.safe_load(TOPOLOGY_PATH.read_text(encoding="utf-8"))


_SAFE_TABLE_NAME = re.compile(r"\A[a-z0-9_]+\Z")


def _assert_safe(name: str) -> str:
    """Defensive guard: every table name is program-generated from
    ``t_NNN`` / ``src_NN_t_NNN`` templates, so a non-matching string here
    means a topology bug, not user input. Raises rather than silently
    interpolating something dangerous into a query."""
    if not _SAFE_TABLE_NAME.match(name):
        raise ValueError(f"unsafe table name: {name!r}")
    return name


def _chunks(items: list[str], size: int) -> list[list[str]]:
    for name in items:
        _assert_safe(name)
    return [items[i : i + size] for i in range(0, len(items), size)]


def _pg_chunk_count(conn: psycopg.Connection, tables: list[str]) -> int:
    quoted = ", ".join(f"'{t}'" for t in tables)
    with conn.cursor() as cur:
        cur.execute(
            f"SELECT COALESCE(SUM(GREATEST(reltuples, 0))::BIGINT, 0) "
            f"FROM pg_class WHERE relname IN ({quoted}) AND relkind = 'r'"
        )
        row = cur.fetchone()
        return int(row[0]) if row else 0


def _pg_chunk_max_id(conn: psycopg.Connection, tables: list[str]) -> int:
    union = " UNION ALL ".join(
        f"SELECT COALESCE(MAX(id), 0) AS m FROM public.{t}" for t in tables
    )
    with conn.cursor() as cur:
        cur.execute(f"SELECT COALESCE(MAX(m), 0) FROM ({union}) u")
        row = cur.fetchone()
        return int(row[0]) if row else 0


def pg_count_and_max(
    conn: psycopg.Connection, table_names: list[str]
) -> tuple[int, int]:
    """Returns (sum_count_via_pg_class, max_id_across_tables).

    Counts are reltuples estimates clamped to >= 0; max ids are exact.
    Queries are chunked so the SQL text stays bounded.
    """
    if not table_names:
        return 0, 0
    total = 0
    max_id = 0
    for chunk in _chunks(table_names, CHUNK_SIZE):
        total += _pg_chunk_count(conn, chunk)
        max_id = max(max_id, _pg_chunk_max_id(conn, chunk))
    return total, max_id


def _ch_query(http_url: str, sql: str) -> str:
    """Run one CH HTTP query with HTTP Basic auth.

    Credentials come from env (gen.py emits them); fall back to ``air:air``
    which is the literal baked into the generated config. The single
    fallback keeps the script runnable against a fresh env without manual
    setup.
    """
    user = os.environ.get("CLICKHOUSE_USER", "air")
    password = os.environ.get("CLICKHOUSE_PASSWORD", "air")
    encoded = urllib.parse.urlencode({"query": sql})
    req = urllib.request.Request(f"{http_url}/?{encoded}", method="GET")
    req.add_header("X-ClickHouse-User", user)
    req.add_header("X-ClickHouse-Key", password)
    with urllib.request.urlopen(req, timeout=10) as resp:
        return resp.read().decode("utf-8", errors="replace")


def ch_count_and_max(
    http_url: str,
    tables_mutable: list[str],
    tables_append: list[str],
) -> tuple[int, int]:
    """Returns (sum_rows, max_id) for one CH host.

    Mutable ReplacingMergeTree tables are counted via ``count() FROM tbl
    FINAL`` so pre-merge duplicate versions don't inflate the number.
    Append-only tables use the cheap ``system.parts.rows`` shortcut.
    """
    if not tables_mutable and not tables_append:
        return 0, 0
    total = 0
    max_id = 0

    # Append-only: cheap per-chunk system.parts aggregation.
    for chunk in _chunks(tables_append, CHUNK_SIZE):
        in_list = ",".join(f"'{t}'" for t in chunk)
        query_rows = (
            f"SELECT toInt64(sum(rows)) FROM system.parts "
            f"WHERE active AND database = 'default' AND table IN ({in_list})"
        )
        total += int(_ch_query(http_url, query_rows).strip() or 0)

    # Mutable: must scan each table with FINAL because system.parts reports
    # the raw pre-dedup row count. Counts are exact at the cost of a brief
    # merge pass per table per tick.
    for chunk in _chunks(tables_mutable, CHUNK_SIZE):
        union = " UNION ALL ".join(
            f"SELECT count() AS c, max(id) AS m FROM {t} FINAL" for t in chunk
        )
        query = (
            f"SELECT toInt64(sum(c)), toInt64(coalesce(max(m), 0)) FROM ({union}) u "
            f"FORMAT TabSeparated"
        )
        out = _ch_query(http_url, query).strip()
        parts = out.split()
        if len(parts) == 2:
            total += int(parts[0] or 0)
            max_id = max(max_id, int(parts[1] or 0))

    # Max id from append tables — cheaper to query MAX(id) than to scan
    # FINAL.
    for chunk in _chunks(tables_append, CHUNK_SIZE):
        union = " UNION ALL ".join(f"SELECT max(id) AS m FROM {t}" for t in chunk)
        query = (
            f"SELECT toInt64(coalesce(max(m), 0)) FROM ({union}) u "
            f"FORMAT TabSeparated"
        )
        v = int(_ch_query(http_url, query).strip() or 0)
        max_id = max(max_id, v)

    return total, max_id


def qdb_count_and_max(
    conn: psycopg.Connection, table_names: list[str]
) -> tuple[int, int]:
    """Returns (sum_count, max_id) for a QuestDB sink via pg-wire."""
    if not table_names:
        return 0, 0
    total = 0
    max_id = 0
    with conn.cursor() as cur:
        for t in table_names:
            cur.execute(f'SELECT count(*), COALESCE(max(id), 0) FROM "{t}"')
            row = cur.fetchone()
            if row:
                total += int(row[0] or 0)
                max_id = max(max_id, int(row[1] or 0))
    return total, max_id


def _format_cell(value: object) -> str:
    if value == "ERR":
        return f"{'ERR':>12s}"
    return f"{int(value):12d}"  # type: ignore[arg-type]


class PgChannel:
    """Lazy pg connection that transparently reopens on drop.

    The validator opens one persistent psycopg.Connection per source /
    sink-pg / sink-qdb at startup. If any one of those backends does a
    crash recovery (e.g. a SIGPIPE on the pg side triggers postmaster
    `terminating any other active server processes`), the persistent
    connection is dead for the rest of the run and every subsequent
    tick reports the backend as ``ERR`` — masking otherwise healthy
    data. This wrapper traps the typical "connection is closed" /
    OperationalError shapes, reopens the socket, and retries once. If
    the second attempt also fails, the caller's existing ERR-sentinel
    path still fires; no silent degradation.
    """

    def __init__(self, url: str, name: str) -> None:
        self.url = url
        self.name = name
        self._conn: psycopg.Connection | None = None

    def _open(self) -> psycopg.Connection:
        if self._conn is None:
            self._conn = psycopg.connect(self.url, autocommit=True)
        return self._conn

    def run(self, work: Any) -> Any:
        """Run ``work(conn)``; if it raises a connection-level error,
        reopen the socket and retry once."""
        try:
            return work(self._open())
        except (psycopg.OperationalError, psycopg.InterfaceError) as exc:
            print(
                f"[validate.py] {self.name} connection dropped, reconnecting: {exc!r}",
                file=sys.stderr,
                flush=True,
            )
            try:
                if self._conn is not None:
                    self._conn.close()
            except Exception:
                pass
            self._conn = None
            # Retry once on the fresh socket. If this also fails, the
            # exception bubbles to the caller's ERR-sentinel handler.
            return work(self._open())

    def close(self) -> None:
        if self._conn is not None:
            try:
                self._conn.close()
            except Exception:
                pass
            self._conn = None


def _drift_free_sleep(interval: float) -> None:
    """Sleep up to ``interval`` seconds, polling ``_STOP`` every 200 ms.

    The previous implementation accumulated 0.2 s chunks and drifted with
    scheduling overhead. This version anchors on monotonic time so the next
    tick fires at ``start + interval`` regardless of poll cost.
    """
    end = time.monotonic() + interval
    while not _STOP:
        remaining = end - time.monotonic()
        if remaining <= 0:
            return
        time.sleep(min(0.2, remaining))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interval", type=float, default=5.0)
    args = parser.parse_args()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    top = load_topology()
    src_count = int(top["sources"]["count"])
    tables_per_source = int(top["sources"]["tables_per_source"])
    pg_count = int(top["sinks"]["postgres"]["count"])
    ch_count = int(top["sinks"]["clickhouse"]["count"])
    qdb_count = int(top["sinks"]["questdb"]["count"])
    mutable_per_source = int(
        top.get("load", {}).get("mutable_tables_per_source", tables_per_source // 2)
    )

    # Routing must match gen.py.route — uses tables_per_source so
    # (src_idx, tbl_idx) is injective into slots.
    def route(src_idx: int, tbl_idx: int) -> tuple[str, int]:
        total = pg_count + ch_count + qdb_count
        slot = (src_idx * tables_per_source + tbl_idx) % total
        if slot < pg_count:
            return ("postgres", slot)
        if slot < pg_count + ch_count:
            return ("clickhouse", slot - pg_count)
        return ("questdb", slot - pg_count - ch_count)

    # Source tables stay plain ``t_NNN`` (one pg per source, no collision).
    # Sink tables are uniquely named ``src_NN_t_NNN`` because sink instances
    # are shared across sources.
    table_names = [f"t_{i:03d}" for i in range(tables_per_source)]
    sink_pg_tables: dict[int, list[str]] = {i: [] for i in range(pg_count)}
    # For CH we split mutable vs append so the count path can pick the
    # right query strategy per table set.
    sink_ch_mutable: dict[int, list[str]] = {i: [] for i in range(ch_count)}
    sink_ch_append: dict[int, list[str]] = {i: [] for i in range(ch_count)}
    sink_qdb_tables: dict[int, list[str]] = {i: [] for i in range(qdb_count)}
    for s in range(src_count):
        for t in range(tables_per_source):
            kind, slot = route(s, t)
            tbl = f"src_{s:02d}_t_{t:03d}"
            is_mutable = t < mutable_per_source
            if kind == "postgres":
                sink_pg_tables[slot].append(tbl)
            elif kind == "clickhouse":
                (sink_ch_mutable if is_mutable else sink_ch_append)[slot].append(tbl)
            else:
                sink_qdb_tables[slot].append(tbl)

    src_channels: list[PgChannel] = []
    sink_pg_channels: list[PgChannel] = []
    sink_qdb_channels: list[PgChannel] = []

    try:
        for i in range(src_count):
            url = os.environ.get(f"SRC_PG_{i:02d}_URL")
            if not url:
                sys.exit(f"[validate.py] SRC_PG_{i:02d}_URL not set")
            src_channels.append(PgChannel(url, f"src_pg_{i:02d}"))
        for i in range(pg_count):
            url = os.environ.get(f"SINK_PG_{i:02d}_URL")
            if not url:
                sys.exit(f"[validate.py] SINK_PG_{i:02d}_URL not set")
            sink_pg_channels.append(PgChannel(url, f"sink_pg_{i:02d}"))
        sink_ch_urls = [
            os.environ.get(f"SINK_CH_{i:02d}_URL", "") for i in range(ch_count)
        ]
        for i in range(qdb_count):
            url = os.environ.get(f"SINK_QDB_{i:02d}_URL")
            if not url:
                sys.exit(f"[validate.py] SINK_QDB_{i:02d}_URL not set")
            sink_qdb_channels.append(PgChannel(url, f"sink_qdb_{i:02d}"))

        print(
            f"[validate.py] sources={src_count} sink_pg={pg_count} sink_ch={ch_count} "
            f"sink_qdb={qdb_count} interval={args.interval}s "
            f"chunk_size={CHUNK_SIZE}",
            flush=True,
        )

        ticks = 0
        t_started = time.monotonic()

        while not _STOP:
            ticks += 1
            per_source_rows: list[tuple[str, object, object]] = []
            for i, ch in enumerate(src_channels):
                try:
                    rows, maxid = ch.run(lambda c: pg_count_and_max(c, table_names))
                    per_source_rows.append((f"src_pg_{i:02d}", rows, maxid))
                except Exception as exc:
                    print(
                        f"[validate.py] ERR src_pg_{i:02d}: {exc!r}",
                        file=sys.stderr,
                        flush=True,
                    )
                    per_source_rows.append((f"src_pg_{i:02d}", "ERR", "ERR"))

            per_sink_rows: list[tuple[str, object, object]] = []
            for i, ch in enumerate(sink_pg_channels):
                try:
                    tables = sink_pg_tables[i]
                    rows, maxid = ch.run(lambda c, _t=tables: pg_count_and_max(c, _t))
                    per_sink_rows.append((f"sink_pg_{i:02d}", rows, maxid))
                except Exception as exc:
                    print(
                        f"[validate.py] ERR sink_pg_{i:02d}: {exc!r}",
                        file=sys.stderr,
                        flush=True,
                    )
                    per_sink_rows.append((f"sink_pg_{i:02d}", "ERR", "ERR"))
            for i, url in enumerate(sink_ch_urls):
                try:
                    rows, maxid = ch_count_and_max(
                        url, sink_ch_mutable[i], sink_ch_append[i]
                    )
                    per_sink_rows.append((f"sink_ch_{i:02d}", rows, maxid))
                except Exception as exc:
                    print(
                        f"[validate.py] ERR sink_ch_{i:02d}: {exc!r}",
                        file=sys.stderr,
                        flush=True,
                    )
                    per_sink_rows.append((f"sink_ch_{i:02d}", "ERR", "ERR"))
            for i, ch in enumerate(sink_qdb_channels):
                try:
                    tables = sink_qdb_tables[i]
                    rows, maxid = ch.run(lambda c, _t=tables: qdb_count_and_max(c, _t))
                    per_sink_rows.append((f"sink_qdb_{i:02d}", rows, maxid))
                except Exception as exc:
                    print(
                        f"[validate.py] ERR sink_qdb_{i:02d}: {exc!r}",
                        file=sys.stderr,
                        flush=True,
                    )
                    per_sink_rows.append((f"sink_qdb_{i:02d}", "ERR", "ERR"))

            stamp = dt.datetime.now(tz=dt.timezone.utc).strftime("%H:%M:%S")
            elapsed = int(time.monotonic() - t_started)
            m, s = divmod(elapsed, 60)
            print(
                f"[validate.py] tick={ticks} t={m:02d}:{s:02d} stamp={stamp}",
                flush=True,
            )
            print("  per_source     rows         max_id", flush=True)
            for name, rows, maxid in per_source_rows:
                print(
                    f"  {name:14s} {_format_cell(rows)} {_format_cell(maxid)}",
                    flush=True,
                )
            print("  per_sink       rows         max_id", flush=True)
            for name, rows, maxid in per_sink_rows:
                print(
                    f"  {name:14s} {_format_cell(rows)} {_format_cell(maxid)}",
                    flush=True,
                )
            # Aggregates exclude any ERR rows so a failing backend doesn't
            # silently inflate or deflate totals/lag.
            total_src = sum(int(r) for _, r, _ in per_source_rows if r != "ERR")
            total_sink = sum(int(r) for _, r, _ in per_sink_rows if r != "ERR")
            src_ids = [int(m) for _, _, m in per_source_rows if m != "ERR"]
            sink_ids = [int(m) for _, _, m in per_sink_rows if m != "ERR"]
            max_src_id = max(src_ids, default=0)
            max_sink_id = max(sink_ids, default=0)
            # Signed lag — negative means sink is AHEAD of source (clock or
            # routing inversion). Clamping to zero would hide that.
            lag = max_src_id - max_sink_id
            err_count = sum(1 for _, r, _ in per_source_rows + per_sink_rows if r == "ERR")
            print(
                f"  TOTAL_SRC={total_src} TOTAL_SINK={total_sink} "
                f"max_src_id={max_src_id} max_sink_id={max_sink_id} "
                f"max_lag={lag} err_rows={err_count}",
                flush=True,
            )

            _drift_free_sleep(args.interval)

        print(f"[validate.py] DONE ticks={ticks}", flush=True)
        return 0
    finally:
        for ch in src_channels + sink_pg_channels + sink_qdb_channels:
            ch.close()


if __name__ == "__main__":
    sys.exit(main())
