# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = []
# ///
"""Tear down the docker-compose stack for the manual mongo → postgres test.

This is the ONLY place containers and volumes are removed — ``run.py``
deliberately leaves them up so iteration between flag combinations is fast.
Cross-platform (auto-detects docker vs podman).
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
    print("[cleanup.py] compose down -v --remove-orphans")
    subprocess.run(
        [*compose, "down", "-v", "--remove-orphans"], cwd=TEST_ROOT, check=True
    )
    for directory in (TEST_ROOT / "logs", TEST_ROOT / ".run-state"):
        if directory.exists():
            print(f"[cleanup.py] removing {directory.relative_to(TEST_ROOT)}/")
            shutil.rmtree(directory)
    print("[cleanup.py] done — containers and volumes are gone")
    return 0


if __name__ == "__main__":
    sys.exit(main())
