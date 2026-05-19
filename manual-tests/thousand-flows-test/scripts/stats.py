# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "psutil==6.1.0",
# ]
# ///
"""Aggregated resource sampler for the thousand-flows-test scaffold.

Sums CPU%/RSS across each *category* of container (src_pg / sink_pg /
sink_ch / sink_qdb / state_pg) rather than emitting a column per
container — at ~18+ containers, per-container TSV is unreadable.

CPU% derivation differs by engine:

* ``podman stats --format json`` returns a JSON array with a
  ``cpu_time`` cumulative-seconds field. We diff that against the
  previous sample to get an instantaneous percentage (100% = one full
  core).
* ``docker stats --format json`` returns JSONL (one object per line)
  with a ``CPUPerc`` field already in percentage form ("12.34%"); we
  use it directly.

Memory units: both engines emit human-readable strings — Docker prefers
the 1024-based IEC suffixes (``MiB``/``GiB``/``TiB``); podman emits SI
suffixes (``MB``/``GB``/``TB``). The unit table covers both.

``--verbose`` re-emits per-container rows every 30 s for debugging.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import signal
import subprocess
import sys
import time
from typing import Any

import psutil

_STOP = False


def _on_signal(_signum: int, _frame: Any) -> None:
    global _STOP
    _STOP = True


# Memory-suffix table: ``stats --format json`` from both engines emits
# human-readable sizes. Covers SI (KB/MB/GB) and IEC (KiB/MiB/GiB). Output
# units are MiB.
_MEM_UNITS = [
    ("TiB", 1024 * 1024),
    ("GiB", 1024),
    ("MiB", 1),
    ("KiB", 1 / 1024),
    ("TB", 1024 * 1024),
    ("GB", 1024),
    ("MB", 1),
    ("KB", 1 / 1024),
    ("B", 1 / 1024 / 1024),
]


def _parse_mem(usage: str) -> float:
    used = usage.split("/")[0].strip()
    for suffix, factor in _MEM_UNITS:
        if used.endswith(suffix):
            try:
                return float(used[: -len(suffix)]) * factor
            except ValueError:
                return 0.0
    return 0.0


_TIME_RE = re.compile(r"(?:(?P<h>\d+)h)?(?:(?P<m>\d+)m)?(?P<s>\d+(?:\.\d+)?)s?")


def _parse_cpu_time(value: Any) -> float:
    """Parse cumulative cpu_time. Accepts numeric (podman ≥ 4.4 ships it
    as a number) or a ``HhMmSs.ss`` string (older podman). Returns
    seconds."""
    if isinstance(value, (int, float)):
        return float(value)
    m = _TIME_RE.fullmatch(str(value).strip())
    if not m:
        return 0.0
    hours = float(m.group("h") or 0)
    minutes = float(m.group("m") or 0)
    seconds = float(m.group("s") or 0)
    return hours * 3600 + minutes * 60 + seconds


def _parse_cpu_perc(value: Any) -> float:
    """Parse docker's ``CPUPerc`` ("12.34%") to a float percentage."""
    s = str(value).strip().rstrip("%").strip()
    try:
        return float(s)
    except ValueError:
        return 0.0


def _run_stats(
    engine: str, names: list[str], timeout: float
) -> list[dict[str, Any]]:
    """Run ``<engine> stats --no-stream --format json`` and return parsed
    rows. Tolerates both JSON-array (podman) and JSONL (docker) output
    shapes. On timeout, kills the child and returns an empty list."""
    try:
        proc = subprocess.run(
            [engine, "stats", "--no-stream", "--format", "json", *names],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        print(
            f"[stats.py] WARNING: `{engine} stats` exceeded {timeout:.0f}s timeout",
            file=sys.stderr,
            flush=True,
        )
        return []
    if proc.returncode != 0:
        return []
    out = proc.stdout.strip()
    if not out:
        return []
    # Try JSON-array first (podman); fall back to JSONL (docker).
    try:
        parsed = json.loads(out)
        if isinstance(parsed, list):
            return parsed
        if isinstance(parsed, dict):
            return [parsed]
    except json.JSONDecodeError:
        pass
    rows: list[dict[str, Any]] = []
    for line in out.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            rows.append(obj)
    return rows


def sample_containers(
    engine: str, names: list[str], timeout: float
) -> dict[str, tuple[float, float, bool]]:
    """Sample container CPU + memory.

    Returns ``{container: (cpu_value, rss_mb, is_pct)}`` where
    ``is_pct=True`` means ``cpu_value`` is already an instantaneous
    percentage (docker), and ``is_pct=False`` means it's a cumulative-
    seconds counter that needs delta-math at the caller (podman).
    """
    if not names:
        return {}
    rows = _run_stats(engine, names, timeout)
    by_name: dict[str, dict[str, Any]] = {}
    for row in rows:
        n = row.get("name") or row.get("Name") or row.get("Container") or ""
        if n:
            by_name[n] = row
    out: dict[str, tuple[float, float, bool]] = {}
    for name in names:
        row = by_name.get(name)
        if row is None:
            out[name] = (0.0, 0.0, False)
            continue
        mem_raw = str(row.get("mem_usage", row.get("MemUsage", "0B / 0B")))
        mem = _parse_mem(mem_raw)
        # Prefer cumulative cpu_time (podman); fall back to CPUPerc (docker).
        if "cpu_time" in row or "CPUTime" in row:
            cpu_time = _parse_cpu_time(row.get("cpu_time", row.get("CPUTime", 0.0)))
            out[name] = (cpu_time, mem, False)
        elif "CPUPerc" in row or "cpu_perc" in row:
            cpu_pct = _parse_cpu_perc(row.get("CPUPerc", row.get("cpu_perc", 0.0)))
            out[name] = (cpu_pct, mem, True)
        else:
            out[name] = (0.0, mem, False)
    return out


def sample_process_cpu_time(pid: int) -> tuple[float, float]:
    try:
        proc = psutil.Process(pid)
        times = proc.cpu_times()
        cpu = times.user + times.system
        rss = proc.memory_info().rss / 1024 / 1024
        return cpu, rss
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        return 0.0, 0.0


def categorise_strict(container_name: str) -> str | None:
    """Trust the compose-naming convention and pull the service token,
    which is everything between the project prefix and the trailing -N."""
    # Compose names: <project>-<service>-1 where service contains _ but not -.
    m = re.match(r".*-(?P<service>[a-z]+_[a-z]+(?:_\d+)?)-\d+$", container_name)
    if not m:
        return None
    service = m.group("service")
    if service == "state_pg":
        return "state_pg"
    for cat in ("src_pg", "sink_pg", "sink_ch", "sink_qdb"):
        if service.startswith(cat + "_"):
            return cat
    return None


CATEGORIES = ["src_pg", "sink_pg", "sink_ch", "sink_qdb", "state_pg"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app-pid", type=int, required=True)
    parser.add_argument("--engine", required=True, help="`podman` or `docker`")
    parser.add_argument(
        "--containers",
        nargs="*",
        default=[],
        help="Full container names (compose pattern: <project>-<service>-1).",
    )
    parser.add_argument("--interval", type=float, default=5.0)
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="Also emit one per-container row every 30s.",
    )
    args = parser.parse_args()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    by_category: dict[str, list[str]] = {c: [] for c in CATEGORIES}
    unknown: list[str] = []
    for name in args.containers:
        cat = categorise_strict(name)
        if cat is None:
            unknown.append(name)
        else:
            by_category[cat].append(name)
    if unknown:
        print(
            f"[stats.py] WARNING: unrecognised containers (skipped from aggregation): {unknown}",
            file=sys.stderr,
            flush=True,
        )

    # `<engine> stats` is supposed to be quick (-- --no-stream returns one
    # snapshot), but a wedged container runtime can hang it indefinitely.
    # We use 2× the sampling interval as the hard cap.
    stats_timeout = max(args.interval * 2.0, 10.0)

    prev_wall = time.monotonic()
    prev_app_cpu, _ = sample_process_cpu_time(args.app_pid)
    initial = sample_containers(args.engine, args.containers, stats_timeout)
    prev_container_cpu: dict[str, float] = {
        name: cpu for name, (cpu, _, is_pct) in initial.items() if not is_pct
    }

    header = ["ts", "app_cpu_pct", "app_rss_mb"]
    for cat in CATEGORIES:
        header += [f"{cat}_cpu_pct", f"{cat}_rss_mb", f"{cat}_n"]
    print("\t".join(header), flush=True)

    last_verbose = 0.0
    while not _STOP:
        slept = 0.0
        while slept < args.interval and not _STOP:
            time.sleep(min(0.2, args.interval - slept))
            slept += 0.2
        if _STOP:
            break

        # Watchdog: if the app process is gone, drain remaining
        # housekeeping and exit. Avoids burning subprocess cycles sampling
        # a dead pid.
        if not psutil.pid_exists(args.app_pid):
            print(
                f"[stats.py] app pid {args.app_pid} no longer exists, exiting",
                file=sys.stderr,
                flush=True,
            )
            break

        now_wall = time.monotonic()
        wall_delta = now_wall - prev_wall

        app_cpu_now, app_rss = sample_process_cpu_time(args.app_pid)
        app_cpu_delta = max(0.0, app_cpu_now - prev_app_cpu)
        app_pct = (app_cpu_delta / wall_delta) * 100.0 if wall_delta > 0 else 0.0

        container_now = sample_containers(args.engine, args.containers, stats_timeout)
        ts = dt.datetime.now(tz=dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        row = [ts, f"{app_pct:.2f}", f"{app_rss:.1f}"]
        for cat in CATEGORIES:
            members = by_category[cat]
            cpu_pct_sum = 0.0
            rss_sum = 0.0
            for name in members:
                cpu_val, mem, is_pct = container_now.get(name, (0.0, 0.0, False))
                if is_pct:
                    pct = cpu_val
                else:
                    cpu_delta = max(0.0, cpu_val - prev_container_cpu.get(name, 0.0))
                    pct = (cpu_delta / wall_delta) * 100.0 if wall_delta > 0 else 0.0
                    prev_container_cpu[name] = cpu_val
                cpu_pct_sum += pct
                rss_sum += mem
            row += [f"{cpu_pct_sum:.2f}", f"{rss_sum:.1f}", str(len(members))]
        print("\t".join(row), flush=True)

        prev_app_cpu = app_cpu_now
        prev_wall = now_wall

        if args.verbose and (now_wall - last_verbose) >= 30.0:
            last_verbose = now_wall
            for name, (cpu, mem, is_pct) in container_now.items():
                kind = "pct" if is_pct else "sec"
                print(
                    f"  [verbose] {name} cpu_{kind}={cpu:.2f} rss_mb={mem:.1f}",
                    flush=True,
                )

    return 0


if __name__ == "__main__":
    sys.exit(main())
