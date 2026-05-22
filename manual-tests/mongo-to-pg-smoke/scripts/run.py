# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "psycopg[binary]==3.2.4",
#   "pymongo==4.10.1",
# ]
# ///
"""Manual end-to-end runner for the mongo → postgres scaffold.

Cross-platform (macOS / Linux / Windows). Uses whichever container CLI is on
PATH — `docker` (which on this host proxies to podman) is tried first, then
`podman` directly. Cleanup of containers and volumes is intentionally NOT
performed here — invoke ``cleanup.py`` afterwards.
"""

from __future__ import annotations

import argparse
import os
import shutil
import signal
import subprocess
import sys
import time
from pathlib import Path

import datetime as dt

import psycopg
from pymongo import MongoClient

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

DEFAULT_MONGO_URL = "mongodb://localhost:27117/appdb"
DEFAULT_PG_SINK_URL = "postgres://air:air@localhost:54322/airdata"
DEFAULT_PG_STATE_URL = "postgres://air:air@localhost:54322/airstate"

LOG_DIR = TEST_ROOT / "logs"
STATE_DIR = TEST_ROOT / ".run-state"

# Compose project name — mirrors docker-compose.yml's `name:` field. Used
# to derive container names without hardcoding them.
COMPOSE_PROJECT = "air-elt-manual-mongo-to-pg-smoke"


def pick_compose_cmd() -> list[str]:
    if shutil.which("docker"):
        return ["docker", "compose"]
    if shutil.which("podman"):
        return ["podman", "compose"]
    sys.exit("[run.py] neither `docker` nor `podman` found on PATH")


COMPOSE = pick_compose_cmd()


def _trim_podman_journal() -> None:
    """Reclaim journald-backed page cache inside the podman VM. Best-effort."""
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


def air_elt_binary() -> Path:
    name = "air-elt.exe" if os.name == "nt" else "air-elt"
    return REPO_ROOT / "target" / "release" / name


def compose(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [*COMPOSE, *args], cwd=TEST_ROOT, check=check, text=True
    )


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


def wait_healthy(service: str, attempts: int = 60, delay: float = 2.0) -> None:
    print(f"[run.py] waiting for {service} to be healthy")
    cid = container_id(service)
    for attempt in range(1, attempts + 1):
        result = subprocess.run(
            [COMPOSE[0], "inspect", "--format={{.State.Health.Status}}", cid],
            capture_output=True,
            text=True,
        )
        status = (result.stdout or "").strip()
        if status == "healthy":
            print(f"[run.py]   {service} healthy")
            return
        if attempt == attempts:
            sys.exit(
                f"[run.py] {service} never reached healthy (last status: {status!r})"
            )
        time.sleep(delay)


def apply_migration(pg_sink_url: str) -> None:
    sql = (TEST_ROOT / "init" / "migrate.sql").read_text(encoding="utf-8")
    print("[run.py] applying migrate.sql to airdata")
    with psycopg.connect(pg_sink_url, autocommit=True) as conn:
        with conn.cursor() as cur:
            cur.execute(sql)


def seed_mongo(mongo_url: str, count: int = 200) -> None:
    """Bulk-insert `count` docs so the mongodb source's schema sampling has
    something to look at. Without this, `air-elt migrate` fails on the
    shared `assemble + validate` pass when the collection is empty. The doc
    shape mirrors load.py so the schema sample covers every typed column."""
    from bson.decimal128 import Decimal128

    print(f"[run.py] seeding {count} docs into appdb.users for schema sampling")
    now = dt.datetime.now(tz=dt.timezone.utc)
    client: MongoClient = MongoClient(mongo_url)
    try:
        coll = client.get_database("appdb").get_collection("users")
        docs = [
            {
                "seq": i,
                "name": f"seed-{i}",
                "email": f"seed-{i}@example.com",
                "is_active": bool(i % 2),
                "age": 18 + (i % 60),
                "score": round((i * 0.137) % 100, 4),
                "balance": Decimal128(f"{(i * 7.13) % 10000:.2f}"),
                "tags": ["alpha", "beta"] if i % 3 else ["gamma"],
                "inserted_at": now,
            }
            for i in range(1, count + 1)
        ]
        coll.insert_many(docs, ordered=False)
    finally:
        client.close()


def spawn_background(name: str, args: list[str], log_path: Path) -> subprocess.Popen[bytes]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    log_file = log_path.open("wb")
    print(f"[run.py] launching {name} -> {log_path.relative_to(TEST_ROOT)}")
    creationflags = 0
    if os.name == "nt":
        # New process group so we can send CTRL_BREAK without hitting the parent.
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
            proc.wait(timeout=5)
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
        help=(
            "How long to keep the air-elt daemon in the foreground (seconds). "
            "0 = run until Ctrl-C (default — true manual mode). Non-zero is "
            "useful for automated smoke runs (≥ 300 to clear the 5-minute floor)."
        ),
    )
    parser.add_argument(
        "--load-rate",
        type=float,
        default=20.0,
        help="Inserts per second for load.py (default 20). Crank to compare cost under heavier RPS.",
    )
    parser.add_argument(
        "--load-duration",
        type=int,
        default=360,
        help="How long load.py keeps inserting (seconds, default 360 — 6 minutes).",
    )
    parser.add_argument(
        "--load-batch-size",
        type=int,
        default=50,
        help=(
            "Docs per bulk_write call in load.py (default 50). Each tick "
            "ships this many ops via bulk_write(ordered=False)."
        ),
    )
    parser.add_argument(
        "--load-update-pct",
        type=int,
        default=20,
        help=(
            "Share of ops in load.py that replay an existing _id via "
            "replace_one(upsert=True), exercising air-elt's conflict path."
        ),
    )
    args = parser.parse_args()

    mongo_url = os.environ.setdefault("MONGO_URL", DEFAULT_MONGO_URL)
    pg_sink_url = os.environ.setdefault("PG_SINK_URL", DEFAULT_PG_SINK_URL)
    os.environ.setdefault("PG_STATE_URL", DEFAULT_PG_STATE_URL)

    LOG_DIR.mkdir(exist_ok=True)
    STATE_DIR.mkdir(exist_ok=True)

    # SIGTERM / SIGHUP raise KeyboardInterrupt so kill <pid> and shell hangup
    # trigger the same teardown path as Ctrl-C.
    def _raise_kb_interrupt(*_args: object) -> None:
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, _raise_kb_interrupt)
    if hasattr(signal, "SIGHUP"):
        signal.signal(signal.SIGHUP, _raise_kb_interrupt)  # type: ignore[attr-defined]

    if COMPOSE[0] == "podman" and sys.platform == "darwin":
        _trim_podman_journal()

    print(f"[run.py] using compose CLI: {' '.join(COMPOSE)}")
    print(f"[run.py] {' '.join(COMPOSE)} up -d")
    compose("up", "-d")
    wait_healthy("mongo")
    wait_healthy("postgres")

    print("[run.py] cargo build --release -p air-elt-app")
    subprocess.run(
        ["cargo", "build", "--release", "-p", "air-elt-app"],
        cwd=REPO_ROOT,
        check=True,
    )

    apply_migration(pg_sink_url)
    seed_mongo(mongo_url)

    binary = air_elt_binary()
    config_arg = "air-elt-config/config.toml"
    print(f"[run.py] {binary} migrate --config {config_arg}")
    subprocess.run(
        [str(binary), "migrate", "--config", config_arg],
        cwd=TEST_ROOT,
        check=True,
    )

    load_proc = spawn_background(
        "load",
        [
            "uv",
            "run",
            "--no-project",
            str(HERE / "load.py"),
            "--url",
            mongo_url,
            "--rate",
            str(args.load_rate),
            "--duration",
            str(args.load_duration),
            "--batch-size",
            str(args.load_batch_size),
            "--update-pct",
            str(args.load_update_pct),
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
    metrics_proc = spawn_background(
        "metrics",
        [
            "uv",
            "run",
            "--no-project",
            str(HERE / "metrics.py"),
            "--interval",
            "5",
        ],
        LOG_DIR / "metrics.log",
    )

    print(
        "\n"
        "================================================================\n"
        "  Tail the background workers in another terminal:\n"
        f"    tail -f {LOG_DIR / 'load.log'}\n"
        f"    tail -f {LOG_DIR / 'validate.log'}\n"
        f"    tail -f {LOG_DIR / 'metrics.log'}\n"
        f"    tail -f {LOG_DIR / 'stats.log'}\n"
        "\n"
        "  After you are done, ALWAYS run `uv run --no-project scripts/cleanup.py`\n"
        "  to drop the containers and volumes — see README.md.\n"
        "================================================================\n"
    )

    print(f"[run.py] starting {binary} run --config {config_arg}")
    air_elt = subprocess.Popen(
        [str(binary), "run", "--config", config_arg],
        cwd=TEST_ROOT,
    )

    container_names = [
        f"{COMPOSE_PROJECT}-{svc}-1" for svc in ("mongo", "postgres")
    ]
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
            print(f"[run.py] bounded run: will stop air-elt after {args.duration}s")
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
        terminate(metrics_proc, "metrics")
        terminate(stats_proc, "stats")
        for pidfile in STATE_DIR.glob("*.pid"):
            pidfile.unlink(missing_ok=True)
        print(
            "[run.py] containers and volumes are STILL UP — run "
            "`uv run --no-project scripts/cleanup.py` to tear them down"
        )

    return exit_code


if __name__ == "__main__":
    sys.exit(main())
