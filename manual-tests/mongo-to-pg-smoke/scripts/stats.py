# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "psutil==6.1.0",
# ]
# ///
"""Resource sampler for the manual mongo → postgres smoke test.

Reports per-container CPU% and RSS, plus the same for the air-elt
process. Engine-aware:

* ``podman stats --format json`` returns a JSON array with a
  ``cpu_time`` cumulative-seconds field — we diff it for instant pct.
* ``docker stats --format json`` returns JSONL with a ``CPUPerc`` field
  in percentage form — we use it directly.

Memory units cover both SI (``MB``/``GB``) and IEC (``MiB``/``GiB``).
``subprocess.run`` has a hard timeout so a wedged container runtime
doesn't hang the loop. The loop exits cleanly when the watched app pid
disappears.
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


# Memory-suffix table: covers SI (KB/MB/GB) and IEC (KiB/MiB/GiB). Output
# in MiB.
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
    s = str(value).strip().rstrip("%").strip()
    try:
        return float(s)
    except ValueError:
        return 0.0


def _run_stats(
    engine: str, names: list[str], timeout: float
) -> list[dict[str, Any]]:
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
    """Returns {container: (cpu_value, rss_mb, is_pct)}.

    ``is_pct=True`` means ``cpu_value`` is an instantaneous percentage
    (docker); ``is_pct=False`` means cumulative-seconds (podman) and the
    caller diffs across ticks.
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
    """Returns (cumulative_cpu_seconds, rss_mb). 0/0 if the PID is gone."""
    try:
        proc = psutil.Process(pid)
        times = proc.cpu_times()
        cpu = times.user + times.system
        rss = proc.memory_info().rss / 1024 / 1024
        return cpu, rss
    except (psutil.NoSuchProcess, psutil.AccessDenied):
        return 0.0, 0.0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app-pid", type=int, required=True)
    parser.add_argument("--engine", required=True, help="`podman` or `docker`")
    parser.add_argument(
        "--containers",
        nargs="*",
        default=[],
        help="Container names to sample (full names, not service names).",
    )
    parser.add_argument("--interval", type=float, default=5.0)
    args = parser.parse_args()

    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    stats_timeout = max(args.interval * 2.0, 10.0)

    prev_wall = time.monotonic()
    prev_app_cpu, _ = sample_process_cpu_time(args.app_pid)
    initial = sample_containers(args.engine, args.containers, stats_timeout)
    prev_container_cpu: dict[str, float] = {
        name: cpu for name, (cpu, _, is_pct) in initial.items() if not is_pct
    }

    header = ["ts", "app_cpu_pct", "app_rss_mb"]
    for name in args.containers:
        short = name.split("-")[-2] if "-" in name else name
        header += [f"{short}_cpu_pct", f"{short}_mem_mb"]
    print("\t".join(header), flush=True)

    while not _STOP:
        slept = 0.0
        while slept < args.interval and not _STOP:
            time.sleep(min(0.2, args.interval - slept))
            slept += 0.2
        if _STOP:
            break

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
        for name in args.containers:
            cpu_val, mem, is_pct = container_now.get(name, (0.0, 0.0, False))
            if is_pct:
                pct = cpu_val
            else:
                cpu_delta = max(0.0, cpu_val - prev_container_cpu.get(name, 0.0))
                pct = (cpu_delta / wall_delta) * 100.0 if wall_delta > 0 else 0.0
                prev_container_cpu[name] = cpu_val
            row += [f"{pct:.2f}", f"{mem:.1f}"]
        print("\t".join(row), flush=True)

        prev_app_cpu = app_cpu_now
        prev_wall = now_wall

    return 0


if __name__ == "__main__":
    sys.exit(main())
