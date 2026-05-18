# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = []
# ///
"""Tear down the docker-compose stack and generated artefacts for the
thousand-flows-test scaffold.

This is the ONLY place containers + volumes + generated files are removed —
``run.py`` deliberately leaves the stack up between iterations. Cross-platform
(auto-detects docker vs podman).
"""

from __future__ import annotations

import shutil
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
TEST_ROOT = HERE.parent


def pick_compose_cmd() -> list[str]:
    if shutil.which("docker"):
        return ["docker", "compose"]
    if shutil.which("podman"):
        return ["podman", "compose"]
    sys.exit("[cleanup.py] neither `docker` nor `podman` found on PATH")


def main() -> int:
    compose = pick_compose_cmd()
    print(f"[cleanup.py] using compose CLI: {' '.join(compose)}")
    if (TEST_ROOT / "docker-compose.yml").exists():
        print("[cleanup.py] compose down -v --remove-orphans")
        result = subprocess.run(
            [*compose, "down", "-v", "--remove-orphans"],
            cwd=TEST_ROOT,
            check=False,
        )
        if result.returncode != 0:
            # Don't shred docker-compose.yml / .env.generated when the
            # `compose down` failed — the operator needs those to retry.
            # Unlinking them would orphan running containers with no way to
            # re-target them by service name.
            print(
                f"[cleanup.py] ERROR: `compose down` failed (rc={result.returncode}). "
                "Skipping deletion of generated files so you can retry. "
                "Investigate the failure, then re-run `cleanup.py`.",
                file=sys.stderr,
                flush=True,
            )
            return result.returncode
    else:
        print("[cleanup.py] no docker-compose.yml — nothing to compose-down")

    paths = [
        TEST_ROOT / "logs",
        TEST_ROOT / ".run-state",
        TEST_ROOT / "init",
        TEST_ROOT / "air-elt-config" / "flows",
        TEST_ROOT / "air-elt-config" / "config.toml",
        TEST_ROOT / "docker-compose.yml",
        TEST_ROOT / ".env.generated",
    ]
    for p in paths:
        if not p.exists():
            continue
        rel = p.relative_to(TEST_ROOT)
        if p.is_dir():
            print(f"[cleanup.py] removing {rel}/")
            shutil.rmtree(p)
        else:
            print(f"[cleanup.py] removing {rel}")
            p.unlink()

    print("[cleanup.py] done — containers, volumes, and generated files are gone")
    return 0


if __name__ == "__main__":
    sys.exit(main())
