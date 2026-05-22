# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "httpx==0.27.2",
# ]
# ///
"""Metrics endpoint checker for the manual mongo → postgres smoke test.

Polls the air-elt `/metrics` endpoint every `--interval` seconds and asserts:

1. The endpoint is reachable and serves `text/plain` Prometheus exposition.
2. Every metric in `REQUIRED_NAMES` shows up (registry wiring intact).
3. The `air_elt_rows_total{stage=read}` and `air_elt_rows_total{stage=written}`
   slices are monotonic non-decreasing across consecutive scrapes — the
   lifecycle contract the runner is supposed to maintain. Filtered out
   of the unified `air_elt_rows_total{stage, op, ...}` family.
4. `process_cpu_seconds_total` is a monotonic counter present in
   every scrape (cumulative CPU seconds; rate gives utilisation).

A single regression — endpoint goes 404, a counter decreases, a
required name disappears — logs an ERR line and increments `--max-errors`.
After `max-errors` consecutive failures the script exits non-zero so an
operator-driven `--duration` run on `run.py` can surface metrics
problems alongside row-lag problems.
"""

from __future__ import annotations

import argparse
import datetime as dt
import os
import re
import signal
import sys
import time
from typing import Any

import httpx

DEFAULT_URL = "http://localhost:8090/metrics"

# Metric families the runner-side wiring is expected to register on
# every scrape (always present once the daemon is up).
ALWAYS_PRESENT = (
    "air_elt_fetch_seconds",
    "air_elt_transform_seconds",
    "air_elt_sink_seconds",
    "air_elt_rows_total",
    "air_elt_lock_max",
    "air_elt_lock_queue_seconds_integral",
    "air_elt_lock_active_seconds_integral",
    # Every connector now mints a `PoolStatsRecorder` unconditionally
    # via its factory; the max/min plain gauges are baked in at mint
    # time. So these families are guaranteed to appear once any flow
    # has been assembled, regardless of whether traffic has flowed.
    "air_elt_pool_connections_max",
    "air_elt_pool_connections_min",
    "flows",
    "sources",
    "sinks",
    "storages",
    "process_cpu_seconds_total",
    "process_resident_memory_bytes",
    "process_start_time_seconds",
    "memory_used_bytes_seconds_integral",
    "memory_available_bytes_seconds_integral",
    "memory_free_bytes_seconds_integral",
    "memory_total_bytes",
    "cpu_count",
)

# `IntCounterVec` families that materialise only after the first
# labelled observation (Prometheus filters empty families from
# `Registry::gather`). The daemon may run cleanly without ever firing
# an error — that's expected, not a bug. The check still pulls these
# when present and feeds their values into the monotonicity audit;
# absence just doesn't fail the script.
LAZY_FAMILIES = (
    "air_elt_errors_total",
)

MONOTONIC = (
    "air_elt_rows_total",
)

# Match a metric line:  name{labels} value
# - For unlabeled metrics: `name value`.
METRIC_LINE = re.compile(
    r"^(?P<name>[a-zA-Z_:][a-zA-Z0-9_:]*)(?:\{[^}]*\})?\s+(?P<value>[-+0-9eE\.\w]+)$"
)

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


def _stamp() -> str:
    return dt.datetime.now(tz=dt.timezone.utc).strftime("%H:%M:%S")


def _scrape_sum(text: str, name: str) -> float:
    """Sum every labelled child of `name`. `_count` and `_sum` suffixes for
    Summary families are NOT chased — those are reserved metric names and
    callers asking about the raw family should pass the bare name."""
    total = 0.0
    saw = False
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if not (line.startswith(name + "{") or line.startswith(name + " ")):
            continue
        m = METRIC_LINE.match(line)
        if m is None or m.group("name") != name:
            continue
        try:
            total += float(m.group("value"))
        except ValueError:
            continue
        saw = True
    return total if saw else float("nan")


def _scrape_sum_filtered(text: str, name: str, want_labels: dict[str, str]) -> float:
    """Sum children of `name` whose labels match every `(k, v)` pair in
    `want_labels`. Used to fold a single `(stage, op)` slice out of the
    unified `air_elt_rows_total` family. Returns 0.0 if the family is
    present but no child matches the filter (vs NaN when the family
    itself is absent) — so the monotonics audit treats "no traffic
    yet" as a legitimate baseline rather than a regression."""
    total = 0.0
    family_seen = False
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if not (line.startswith(name + "{") or line.startswith(name + " ")):
            continue
        m = METRIC_LINE.match(line)
        if m is None or m.group("name") != name:
            continue
        family_seen = True
        labels_section = ""
        brace = line.find("{")
        if brace != -1:
            close = line.rfind("}")
            if close > brace:
                labels_section = line[brace + 1 : close]
        if not all(
            f'{k}="{v}"' in labels_section for k, v in want_labels.items()
        ):
            continue
        try:
            total += float(m.group("value"))
        except ValueError:
            continue
    return total if family_seen else float("nan")


def _names_present(text: str) -> set[str]:
    """Return the set of `# TYPE …` declarations in the exposition. Using
    TYPE rather than HELP because some libraries omit HELP for derived
    families — TYPE is mandatory per Prom exposition spec."""
    found: set[str] = set()
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("# TYPE "):
            parts = line.split()
            if len(parts) >= 3:
                found.add(parts[2])
    return found


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--url",
        default=os.environ.get("METRICS_URL", DEFAULT_URL),
        help="Metrics endpoint URL (default %(default)s)",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=5.0,
        help="Seconds between scrapes (default 5).",
    )
    parser.add_argument(
        "--max-errors",
        type=int,
        default=3,
        help=(
            "Consecutive scrape failures before exiting non-zero. "
            "The first scrape can fail because the daemon is still "
            "booting — a small budget tolerates the warm-up window."
        ),
    )
    args = parser.parse_args()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    print(
        f"[metrics.py] url={args.url} interval={args.interval}s "
        f"max_errors={args.max_errors}",
        flush=True,
    )

    consecutive_errors = 0
    prev_rows_read = float("nan")
    prev_rows_written = float("nan")
    client = httpx.Client(timeout=5.0)

    try:
        while not _STOP:
            try:
                response = client.get(args.url)
            except httpx.HTTPError as exc:
                consecutive_errors += 1
                print(
                    f"[metrics.py] {_stamp()} ERR fetch failed: {exc!r} "
                    f"(consecutive={consecutive_errors})",
                    file=sys.stderr,
                    flush=True,
                )
                if consecutive_errors >= args.max_errors:
                    print("[metrics.py] giving up", file=sys.stderr, flush=True)
                    return 1
                _drift_free_sleep(args.interval)
                continue

            if response.status_code != 200:
                consecutive_errors += 1
                print(
                    f"[metrics.py] {_stamp()} ERR status={response.status_code} "
                    f"(consecutive={consecutive_errors})",
                    file=sys.stderr,
                    flush=True,
                )
                if consecutive_errors >= args.max_errors:
                    return 1
                _drift_free_sleep(args.interval)
                continue

            content_type = response.headers.get("content-type", "")
            if not content_type.startswith("text/plain"):
                print(
                    f"[metrics.py] {_stamp()} ERR unexpected content-type: {content_type}",
                    file=sys.stderr,
                    flush=True,
                )
                consecutive_errors += 1
                if consecutive_errors >= args.max_errors:
                    return 1
                _drift_free_sleep(args.interval)
                continue

            text = response.text
            present = _names_present(text)
            missing = [name for name in ALWAYS_PRESENT if name not in present]
            lazy_seen = [name for name in LAZY_FAMILIES if name in present]
            if missing:
                consecutive_errors += 1
                print(
                    f"[metrics.py] {_stamp()} ERR missing metric families: {missing}",
                    file=sys.stderr,
                    flush=True,
                )
                if consecutive_errors >= args.max_errors:
                    return 1
                _drift_free_sleep(args.interval)
                continue

            # The unified `air_elt_rows_total` family carries every
            # `(stage, op)` combination. Filter by `stage` label so the
            # `rows_read` / `rows_written` semantic survives the family
            # consolidation.
            rows_read = _scrape_sum_filtered(text, "air_elt_rows_total", {"stage": "read"})
            rows_written = _scrape_sum_filtered(text, "air_elt_rows_total", {"stage": "written"})
            # `process_cpu_seconds_total` is a monotonic counter — the
            # printed value grows scrape-over-scrape (operators read
            # utilisation via `rate()`, not the raw value).
            cpu_now = _scrape_sum(text, "process_cpu_seconds_total")
            monotonic_ok = True
            if prev_rows_read == prev_rows_read and rows_read < prev_rows_read:
                print(
                    f"[metrics.py] {_stamp()} ERR rows_read decreased: "
                    f"{prev_rows_read} -> {rows_read}",
                    file=sys.stderr,
                    flush=True,
                )
                monotonic_ok = False
            if prev_rows_written == prev_rows_written and rows_written < prev_rows_written:
                print(
                    f"[metrics.py] {_stamp()} ERR rows_written decreased: "
                    f"{prev_rows_written} -> {rows_written}",
                    file=sys.stderr,
                    flush=True,
                )
                monotonic_ok = False
            if not monotonic_ok:
                consecutive_errors += 1
                if consecutive_errors >= args.max_errors:
                    return 1
                _drift_free_sleep(args.interval)
                continue

            prev_rows_read = rows_read
            prev_rows_written = rows_written
            consecutive_errors = 0
            lazy_tag = f" lazy={','.join(lazy_seen)}" if lazy_seen else ""
            print(
                f"[metrics.py] {_stamp()} ok families={len(present)} "
                f"rows_read={rows_read:.0f} rows_written={rows_written:.0f} "
                f"cpu={cpu_now:.1f}{lazy_tag}",
                flush=True,
            )
            _drift_free_sleep(args.interval)
    finally:
        client.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
