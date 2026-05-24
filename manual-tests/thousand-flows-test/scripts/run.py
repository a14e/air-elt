# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "psycopg[binary]==3.2.4",
#   "PyYAML==6.0.2",
# ]
# ///
"""Manual end-to-end runner for the thousand-flows-test scaffold.

Sequence:
  1. (Re)generate compose, config, init SQL, flow .toml files via gen.py.
  2. compose up -d.
  3. Wait for every container's healthcheck (count derived from
     topology.yaml at run time; current default ~18).
  4. cargo build --release -p air-elt-app (unless --skip-build).
  5. air-elt migrate --config air-elt-config/config.toml.
  6. Spawn load.py + validate.py + stats.py in background.
  7. Spawn `air-elt run` in the foreground; wait for --duration or Ctrl-C.
  8. Terminate background workers. DO NOT compose down — cleanup.py does that.

Cross-platform: auto-detects docker vs podman compose. Under
podman+macOS the orchestrator additionally trims journald-backed page
cache in the VM and warns if the VM has < 12 GiB of RAM (the working set
across the stack lands in the 10-13 GiB band).
"""

from __future__ import annotations

import argparse
import concurrent.futures
import os
import shutil
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

import yaml

HERE = Path(__file__).resolve().parent
TEST_ROOT = HERE.parent
REPO_ROOT = Path(
    subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        cwd=HERE,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
)

LOG_DIR = TEST_ROOT / "logs"
STATE_DIR = TEST_ROOT / ".run-state"
TOPOLOGY_PATH = TEST_ROOT / "topology.yaml"

COMPOSE_PROJECT = "air-elt-manual-thousand-flows-test"


def pick_compose_cmd() -> list[str]:
    if shutil.which("docker"):
        return ["docker", "compose"]
    if shutil.which("podman"):
        return ["podman", "compose"]
    sys.exit("[run.py] neither `docker` nor `podman` found on PATH")


COMPOSE = pick_compose_cmd()

# Recommended floor for the podman-machine VM at the default thousand-flows
# topology. Below this, sink containers OOM-kill once data starts flowing.
PODMAN_MIN_MEMORY_GIB = 12


def _trim_podman_journal() -> None:
    """Reclaim journald-backed page cache inside the podman VM.

    libkrun on Apple Silicon holds the guest's RSS as the krunkit host RSS;
    a stale journal at the 4 GiB cap pins ~3.7 GiB of cache that macOS
    Activity Monitor (and OOM heuristics) treat as "used". Vacuuming the
    journal frees both disk and the cache pages it kept warm. Best-effort:
    a failure here doesn't block the run.
    """
    print(
        "[run.py] podman: vacuuming journald inside the machine "
        "(`journalctl --vacuum-size=200M`)",
        flush=True,
    )
    try:
        subprocess.run(
            [
                "podman",
                "machine",
                "ssh",
                "--",
                "sudo",
                "journalctl",
                "--vacuum-size=200M",
            ],
            check=False,
            timeout=30,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError) as exc:
        print(f"[run.py]   journal vacuum skipped: {exc}", flush=True)


def _check_podman_memory_cap() -> None:
    """Warn if the podman-machine VM has less than ``PODMAN_MIN_MEMORY_GIB``.

    Parses ``podman machine inspect`` JSON; on any parse hiccup, falls back
    silently — the warning is best-effort guidance, not a precondition.
    """
    try:
        out = subprocess.run(
            ["podman", "machine", "inspect"],
            check=True,
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout
        import json as _json

        machines = _json.loads(out)
        if not isinstance(machines, list) or not machines:
            return
        first = machines[0]
        memory_bytes = (
            first.get("Resources", {}).get("Memory")
            or first.get("ConfigDir", {}).get("Memory")
            or 0
        )
        # `podman machine inspect` reports memory in MiB on some podman
        # versions and bytes on others. Heuristic: if the number is small
        # (< 1e6), it's MiB; otherwise bytes.
        if memory_bytes < 1_000_000:
            memory_gib = memory_bytes / 1024.0
        else:
            memory_gib = memory_bytes / (1024.0 * 1024.0 * 1024.0)
        if memory_gib < PODMAN_MIN_MEMORY_GIB:
            print(
                f"[run.py] WARNING: podman machine has {memory_gib:.1f} GiB RAM; "
                f"recommend >= {PODMAN_MIN_MEMORY_GIB} GiB for thousand-flows. "
                "Run `podman machine set --memory 16384` and restart the machine.",
                flush=True,
            )
    except Exception:
        # Best-effort: any failure (no machine, parse error, etc.) is fine.
        pass


def air_elt_binary() -> Path:
    name = "air-elt.exe" if os.name == "nt" else "air-elt"
    return REPO_ROOT / "target" / "release" / name


def compose(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run([*COMPOSE, *args], cwd=TEST_ROOT, check=check, text=True)


def container_id(service: str) -> str:
    out = subprocess.run(
        [*COMPOSE, "ps", "-q", service],
        cwd=TEST_ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if not out:
        sys.exit(f"[run.py] no container id for service {service!r}")
    return out.splitlines()[0]


def wait_healthy_one(service: str, attempts: int = 90, delay: float = 2.0) -> str:
    cid = container_id(service)
    for attempt in range(1, attempts + 1):
        result = subprocess.run(
            [COMPOSE[0], "inspect", "--format={{.State.Health.Status}}", cid],
            capture_output=True,
            text=True,
        )
        status = (result.stdout or "").strip()
        if status == "healthy":
            return service
        if attempt == attempts:
            raise RuntimeError(
                f"[run.py] {service} never reached healthy (last status: {status!r})"
            )
        time.sleep(delay)
    return service


def wait_all_healthy(services: list[str]) -> None:
    print(f"[run.py] waiting for {len(services)} containers to be healthy")
    with concurrent.futures.ThreadPoolExecutor(max_workers=min(len(services), 16)) as ex:
        futures = {ex.submit(wait_healthy_one, s): s for s in services}
        done = 0
        for fut in concurrent.futures.as_completed(futures):
            service = futures[fut]
            try:
                fut.result()
            except Exception as exc:
                sys.exit(f"[run.py] {service}: {exc}")
            done += 1
            print(f"[run.py]   healthy [{done}/{len(services)}]: {service}")


def load_topology() -> dict:
    return yaml.safe_load(TOPOLOGY_PATH.read_text(encoding="utf-8"))


def warm_up_pg_pools(top: dict) -> None:
    """After compose-healthcheck passes, every pg container still has a brief
    cold-listen-backlog window where bursts of concurrent connects can be
    TCP-reset. This walks each pg port, opens 5 short-lived connections in
    sequence, runs `SELECT 1`, closes — gives pg enough warm-up for the
    bgwriter/wal-writer/autovacuum-launcher init dance and for the catalog
    pages to land in shared_buffers. Cheap insurance against the
    `Connection reset by peer` we hit on the first `air-elt migrate` call."""
    import psycopg  # local import; only needed when warming

    src_count = int(top["sources"]["count"])
    pg_sink_count = int(top["sinks"]["postgres"]["count"])
    src_base = int(top["ports"].get("source_pg_base", 55100))
    sink_base = int(top["ports"].get("sink_pg_base", 55200))
    state_port = int(top["ports"].get("state_pg", 55300))

    targets = []
    for i in range(src_count):
        targets.append((f"src_pg_{i:02d}", src_base + i, "airdata"))
    for i in range(pg_sink_count):
        targets.append((f"sink_pg_{i:02d}", sink_base + i, "airdata"))
    targets.append(("state_pg", state_port, "airstate"))

    print(
        f"[run.py] warming up {len(targets)} pg pools (5 connections each, "
        "with retry on cold-start reset)",
        flush=True,
    )
    for name, port, db in targets:
        url = f"postgres://air:air@localhost:{port}/{db}"
        for attempt_idx in range(5):
            backoff = 0.25
            while True:
                try:
                    with psycopg.connect(url, autocommit=True, connect_timeout=10) as conn:
                        with conn.cursor() as cur:
                            cur.execute("SELECT 1")
                    break
                except psycopg.OperationalError as e:
                    if backoff > 60.0:
                        raise RuntimeError(
                            f"warm-up of {name} (port={port}) keeps failing: {e}"
                        ) from e
                    print(
                        f"[run.py]   {name} cold-start, retrying in {backoff:.2f}s: {e}",
                        flush=True,
                    )
                    time.sleep(backoff)
                    backoff *= 2.0
        print(f"[run.py]   warmed {name}", flush=True)


def apply_questdb_init(top: dict) -> None:
    """QuestDB has no `/docker-entrypoint-initdb.d/` equivalent, so the init
    SQL is POSTed via the REST `/exec` endpoint after the container is up."""
    qdb_count = int(top["sinks"]["questdb"]["count"])
    if qdb_count == 0:
        return
    http_base = int(top["ports"].get("questdb_http_base", 9400))
    init_dir = TEST_ROOT / "init" / "sink_qdb"
    for i in range(qdb_count):
        sql_file = init_dir / f"snk{i:02d}.sql"
        if not sql_file.exists():
            sys.exit(f"[run.py] missing QuestDB init SQL: {sql_file}")
        port = http_base + i
        statements = [s.strip() for s in sql_file.read_text(encoding="utf-8").split(";") if s.strip()]
        print(f"[run.py] applying {len(statements)} statements to sink_qdb_{i:02d} via http://localhost:{port}")
        for stmt in statements:
            url = f"http://localhost:{port}/exec?query={urllib.parse.quote(stmt)}"
            req = urllib.request.Request(url)
            req.add_header("Authorization", "Basic YWlyOmFpcg==")  # air:air
            try:
                with urllib.request.urlopen(req, timeout=15) as resp:
                    resp.read()
            except urllib.error.HTTPError as e:
                sys.exit(f"[run.py] questdb init failed on sink_qdb_{i:02d}: {e.code} {e.read().decode(errors='replace')}\nstatement: {stmt[:200]}")


def services_from_topology(top: dict) -> list[str]:
    src = int(top["sources"]["count"])
    sinks = top["sinks"]
    pg = int(sinks["postgres"]["count"])
    ch = int(sinks["clickhouse"]["count"])
    qdb = int(sinks["questdb"]["count"])
    services = [f"src_pg_{i:02d}" for i in range(src)]
    services += [f"sink_pg_{i:02d}" for i in range(pg)]
    services += [f"sink_ch_{i:02d}" for i in range(ch)]
    services += [f"sink_qdb_{i:02d}" for i in range(qdb)]
    services.append("state_pg")
    return services


def export_env_from_generated() -> None:
    env_file = TEST_ROOT / ".env.generated"
    if not env_file.exists():
        sys.exit("[run.py] .env.generated missing — did gen.py run?")
    for raw in env_file.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("export "):
            line = line[len("export "):]
        if "=" not in line:
            continue
        key, _, value = line.partition("=")
        value = value.strip().strip('"').strip("'")
        os.environ[key.strip()] = value


def spawn_background(name: str, args: list[str], log_path: Path) -> subprocess.Popen[bytes]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_file = log_path.open("wb")
    print(f"[run.py] launching {name} -> {log_path.relative_to(TEST_ROOT)}")
    creationflags = 0
    if os.name == "nt":
        creationflags = subprocess.CREATE_NEW_PROCESS_GROUP  # type: ignore[attr-defined]
    proc = subprocess.Popen(
        args,
        cwd=TEST_ROOT,
        stdout=log_file,
        stderr=subprocess.STDOUT,
        creationflags=creationflags,
    )
    (STATE_DIR / f"{name}.pid").write_text(str(proc.pid), encoding="utf-8")
    return proc


def terminate(proc: subprocess.Popen[bytes], name: str) -> None:
    if proc.poll() is not None:
        return
    print(f"[run.py] stopping {name} (pid {proc.pid})")
    try:
        if os.name == "nt":
            proc.send_signal(signal.CTRL_BREAK_EVENT)  # type: ignore[attr-defined]
        else:
            proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    except ProcessLookupError:
        pass


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--duration",
        type=int,
        default=0,
        help="How long to keep `air-elt run` in the foreground (seconds). 0 = until Ctrl-C.",
    )
    parser.add_argument(
        "--skip-gen", action="store_true", help="Skip running gen.py."
    )
    parser.add_argument(
        "--skip-build", action="store_true", help="Skip `cargo build --release`."
    )
    parser.add_argument(
        "--setup-only",
        action="store_true",
        help="Run only the setup phase (gen, compose up, build, migrate) then exit.",
    )
    parser.add_argument(
        "--run-only",
        action="store_true",
        help="Skip setup, run air-elt with background workers (assumes setup was done).",
    )
    args = parser.parse_args()

    LOG_DIR.mkdir(exist_ok=True)
    STATE_DIR.mkdir(exist_ok=True)

    # SIGTERM / SIGHUP raise KeyboardInterrupt so the same teardown path as
    # Ctrl-C fires when the orchestrator is killed by `kill <pid>` or the
    # parent shell hangs up. Windows lacks SIGHUP.
    def _raise_kb_interrupt(*_args: object) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, _raise_kb_interrupt)
    if hasattr(signal, "SIGHUP"):
        signal.signal(signal.SIGHUP, _raise_kb_interrupt)  # type: ignore[attr-defined]

    if COMPOSE[0] == "podman" and sys.platform == "darwin":
        _trim_podman_journal()
        _check_podman_memory_cap()

    if not args.run_only:
        if not args.skip_gen:
            print("[run.py] running gen.py")
            subprocess.run(
                ["uv", "run", "--no-project", str(HERE / "gen.py")],
                cwd=TEST_ROOT,
                check=True,
            )

    export_env_from_generated()
    top = load_topology()

    if not args.run_only:
        services = services_from_topology(top)

        print(f"[run.py] using compose CLI: {' '.join(COMPOSE)}")
        print(f"[run.py] {' '.join(COMPOSE)} up -d ({len(services)} services)")
        compose("up", "-d")
        wait_all_healthy(services)

        if not args.skip_build:
            print("[run.py] cargo build --release -p air-elt-app")
            subprocess.run(
                ["cargo", "build", "--release", "-p", "air-elt-app"],
                cwd=REPO_ROOT,
                check=True,
            )

        warm_up_pg_pools(top)
        apply_questdb_init(top)

        binary = air_elt_binary()
        config_arg = "air-elt-config/config.toml"
        migrate_log_path = LOG_DIR / "air-elt-migrate.log"
        print(f"[run.py] {binary} migrate --config {config_arg} -> {migrate_log_path}")
        with migrate_log_path.open("wb") as mlog:
            subprocess.run(
                [str(binary), "migrate", "--config", config_arg],
                cwd=TEST_ROOT,
                check=True,
                stdout=mlog,
                stderr=subprocess.STDOUT,
            )

        if args.setup_only:
            print("[run.py] setup complete (--setup-only)")
            return 0

    binary = air_elt_binary()
    config_arg = "air-elt-config/config.toml"
    load = top.get("load", {})
    load_proc = spawn_background(
        "load",
        [
            "uv",
            "run",
            "--no-project",
            str(HERE / "load.py"),
            "--duration",
            str(load.get("duration_secs", 600)),
        ],
        LOG_DIR / "load.log",
    )
    validate_proc = spawn_background(
        "validate",
        [
            "uv",
            "run",
            "--no-project",
            str(HERE / "validate.py"),
            "--interval",
            "5",
        ],
        LOG_DIR / "validate.log",
    )

    print(
        "\n"
        "================================================================\n"
        "  Tail the background workers in another terminal:\n"
        f"    tail -f {LOG_DIR / 'load.log'}\n"
        f"    tail -f {LOG_DIR / 'validate.log'}\n"
        f"    tail -f {LOG_DIR / 'stats.log'}\n"
        "\n"
        "  After you are done, ALWAYS run `uv run --no-project scripts/cleanup.py`\n"
        "  to drop the containers and volumes — see README.md.\n"
        "================================================================\n"
    )

    air_elt_log_path = LOG_DIR / "air-elt.log"
    print(f"[run.py] starting {binary} run --config {config_arg} -> {air_elt_log_path}")
    air_elt_log = air_elt_log_path.open("wb")
    air_elt = subprocess.Popen(
        [str(binary), "run", "--config", config_arg],
        cwd=TEST_ROOT,
        stdout=air_elt_log,
        stderr=subprocess.STDOUT,
    )

    container_names = [f"{COMPOSE_PROJECT}-{s}-1" for s in services]
    stats_proc = spawn_background(
        "stats",
        [
            "uv",
            "run",
            "--no-project",
            str(HERE / "stats.py"),
            "--app-pid",
            str(air_elt.pid),
            "--engine",
            COMPOSE[0],
            "--interval",
            "5",
            "--containers",
            *container_names,
        ],
        LOG_DIR / "stats.log",
    )

    exit_code = 0
    try:
        if args.duration > 0:
            print(f"[run.py] bounded run: stopping air-elt after {args.duration}s")
            try:
                exit_code = air_elt.wait(timeout=args.duration)
            except subprocess.TimeoutExpired:
                print("[run.py] duration elapsed, terminating air-elt")
                terminate(air_elt, "air-elt")
                exit_code = 0
        else:
            exit_code = air_elt.wait()
    except KeyboardInterrupt:
        print("\n[run.py] Ctrl-C received")
        terminate(air_elt, "air-elt")
    finally:
        terminate(load_proc, "load")
        terminate(validate_proc, "validate")
        terminate(stats_proc, "stats")
        try:
            air_elt_log.close()
        except Exception:
            pass
        for pidfile in STATE_DIR.glob("*.pid"):
            pidfile.unlink(missing_ok=True)
        print(
            "[run.py] DONE — containers and volumes are STILL UP. Run "
            "`uv run --no-project scripts/cleanup.py` to tear them down."
        )

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
