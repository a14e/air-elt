# /// script
# requires-python = ">=3.11,<3.14"
# dependencies = [
#   "PyYAML==6.0.2",
# ]
# ///
"""Generator for the thousand-flows-test scaffold.

Reads ``topology.yaml`` from the test root and (re)writes:

* ``docker-compose.yml``
* ``init/source_pg/srcNN.sql`` (one per source pg)
* ``init/sink_pg/snkNN.sql``  (one per sink pg)
* ``init/sink_ch/snkNN.sql``  (one per sink CH)
* ``init/state_pg/00-create-databases.sql``
* ``air-elt-config/config.toml``
* ``air-elt-config/flows/srcNN_tblNNN.toml`` (1000 by default)
* ``.env.generated``

Idempotent: wipes ``air-elt-config/flows/`` and ``init/*`` before regenerating
so shrinking ``topology.yaml`` does not leave stale files.

Only dependency: ``PyYAML``. TOML/YAML/SQL output is hand-formatted templates
so we keep the dep surface small.
"""

from __future__ import annotations

import argparse
import shutil
import sys
from pathlib import Path
from typing import Any

import yaml

HERE = Path(__file__).resolve().parent
TEST_ROOT = HERE.parent

TOPOLOGY_PATH = TEST_ROOT / "topology.yaml"
COMPOSE_PATH = TEST_ROOT / "docker-compose.yml"
CONFIG_PATH = TEST_ROOT / "air-elt-config" / "config.toml"
FLOWS_DIR = TEST_ROOT / "air-elt-config" / "flows"
INIT_DIR = TEST_ROOT / "init"
ENV_PATH = TEST_ROOT / ".env.generated"

COMPOSE_PROJECT = "air-elt-manual-thousand-flows-test"

# ---------------------------------------------------------------------------
# routing
# ---------------------------------------------------------------------------


def route(
    src_idx: int,
    tbl_idx: int,
    tables_per_source: int,
    pg_count: int,
    ch_count: int,
    qdb_count: int,
) -> tuple[str, int]:
    """Deterministic sink picker.

    A flow with ``(src_idx, tbl_idx)`` is assigned to one of the
    ``pg_count + ch_count + qdb_count`` sinks via a stable index. The
    multiplier ``tables_per_source`` makes the index injective across
    ``(src_idx, tbl_idx)`` — using a smaller constant (e.g. 100) collapses
    different flows to the same slot once ``tbl_idx >= 100``, which is what
    happens at ``tables_per_source=1000``.
    Slot layout: postgres slots first, then clickhouse, then questdb.
    """
    total = pg_count + ch_count + qdb_count
    slot = (src_idx * tables_per_source + tbl_idx) % total
    if slot < pg_count:
        return ("postgres", slot)
    if slot < pg_count + ch_count:
        return ("clickhouse", slot - pg_count)
    return ("questdb", slot - pg_count - ch_count)


def is_heavy(tbl_idx: int, heavy_per_src: int) -> bool:
    return tbl_idx < heavy_per_src


def is_mutable(tbl_idx: int, mutable_per_src: int) -> bool:
    """Tables with ``tbl_idx < mutable_per_src`` receive UPDATE traffic from
    ``load.py`` and (on the sink side) get conflict/dedup wiring; the rest
    are append-only."""
    return tbl_idx < mutable_per_src


# ---------------------------------------------------------------------------
# topology loader + validation
# ---------------------------------------------------------------------------


def _require(d: dict[str, Any], key: str, where: str) -> Any:
    if key not in d:
        sys.exit(f"[gen.py] topology.yaml: missing '{key}' in {where}")
    return d[key]


def load_topology(path: Path) -> dict[str, Any]:
    if not path.exists():
        sys.exit(f"[gen.py] topology.yaml not found at {path}")
    raw = yaml.safe_load(path.read_text(encoding="utf-8"))
    if not isinstance(raw, dict):
        sys.exit("[gen.py] topology.yaml: top-level must be a mapping")

    sources = _require(raw, "sources", "topology")
    src_count = int(_require(sources, "count", "sources"))
    tables_per_source = int(_require(sources, "tables_per_source", "sources"))
    heavy_per_source = int(_require(sources, "heavy_tables_per_source", "sources"))

    sinks = _require(raw, "sinks", "topology")
    pg_count = int(_require(_require(sinks, "postgres", "sinks"), "count", "sinks.postgres"))
    ch_count = int(_require(_require(sinks, "clickhouse", "sinks"), "count", "sinks.clickhouse"))
    qdb_count = int(_require(_require(sinks, "questdb", "sinks"), "count", "sinks.questdb"))

    if src_count < 1:
        sys.exit("[gen.py] sources.count must be >= 1")
    if tables_per_source < 1:
        sys.exit("[gen.py] sources.tables_per_source must be >= 1")
    if heavy_per_source < 0 or heavy_per_source > tables_per_source:
        sys.exit("[gen.py] sources.heavy_tables_per_source must be in [0, tables_per_source]")
    if pg_count + ch_count + qdb_count < 1:
        sys.exit("[gen.py] need at least one sink (postgres + clickhouse + questdb combined)")

    flow = raw.get("flow", {}) or {}
    batch_limit = int(flow.get("batch_limit", 512))
    if batch_limit < 1:
        sys.exit("[gen.py] flow.batch_limit must be >= 1")
    if batch_limit * 12 > 60_000:
        sys.exit(
            f"[gen.py] flow.batch_limit={batch_limit} × 12 columns > 60k pg-param cap; reduce."
        )

    # We deliberately emit one explicit flow per (source, table) — no wildcards
    # — so air-elt's WildcardUniverseTooLarge cap (4096) does not apply.
    # Surfacing a warning every run masked real issues, so it is suppressed.

    ports = raw.get("ports", {}) or {}
    src_pg_base = int(ports.get("source_pg_base", 55100))
    sink_pg_base = int(ports.get("sink_pg_base", 55200))
    sink_ch_http_base = int(ports.get("sink_ch_http_base", 8200))
    sink_ch_tcp_base = int(ports.get("sink_ch_tcp_base", 9200))
    questdb_pg_base = int(ports.get("questdb_pg_base", 55400))
    questdb_http_base = int(ports.get("questdb_http_base", 9400))
    state_pg = int(ports.get("state_pg", 55300))

    used: list[tuple[str, int]] = []
    for label, base, n in [
        ("source_pg", src_pg_base, src_count),
        ("sink_pg", sink_pg_base, pg_count),
        ("sink_ch_http", sink_ch_http_base, ch_count),
        ("sink_ch_tcp", sink_ch_tcp_base, ch_count),
        ("questdb_pg", questdb_pg_base, qdb_count),
        ("questdb_http", questdb_http_base, qdb_count),
    ]:
        for i in range(n):
            used.append((label, base + i))
    used.append(("state_pg", state_pg))
    # check overlap with existing scaffolds + with itself
    known_used = {27117, 54322}
    seen: dict[int, str] = {}
    for label, port in used:
        if port in known_used:
            sys.exit(
                f"[gen.py] port {port} ({label}) clashes with existing manual-tests/mongo-to-pg-smoke "
                "scaffold (uses 27117 / 54322). Adjust ports.* in topology.yaml."
            )
        if port in seen:
            sys.exit(
                f"[gen.py] port {port} double-booked: {seen[port]} vs {label}. "
                "Spread the port bases apart in topology.yaml."
            )
        seen[port] = label

    load = raw.get("load", {}) or {}
    resources = raw.get("resources", {}) or {}
    healthcheck = raw.get("healthcheck", {}) or {}

    mutable_per_source = int(
        load.get("mutable_tables_per_source", tables_per_source // 2)
    )
    if mutable_per_source < 0 or mutable_per_source > tables_per_source:
        sys.exit(
            "[gen.py] load.mutable_tables_per_source must be in [0, tables_per_source]"
        )
    update_pct = int(load.get("update_pct", 20))
    if update_pct < 0 or update_pct > 100:
        sys.exit("[gen.py] load.update_pct must be in [0, 100]")

    return {
        "src_count": src_count,
        "tables_per_source": tables_per_source,
        "heavy_per_source": heavy_per_source,
        "mutable_per_source": mutable_per_source,
        "pg_count": pg_count,
        "ch_count": ch_count,
        "qdb_count": qdb_count,
        "src_pg_base": src_pg_base,
        "sink_pg_base": sink_pg_base,
        "sink_ch_http_base": sink_ch_http_base,
        "sink_ch_tcp_base": sink_ch_tcp_base,
        "questdb_pg_base": questdb_pg_base,
        "questdb_http_base": questdb_http_base,
        "state_pg": state_pg,
        "batch_limit": batch_limit,
        "cursor_interval": str(flow.get("cursor_interval", "1s")),
        "cursor_jitter": str(flow.get("cursor_jitter", "")),
        "query_timeout": str(flow.get("query_timeout", "30s")),
        "heavy_rps": float(load.get("heavy_rps", 20)),
        "light_rps": float(load.get("light_rps", 0.5)),
        "duration_secs": int(load.get("duration_secs", 600)),
        "batch_size": int(load.get("batch_size", 8)),
        "update_pct": update_pct,
        "pg_memory": str(resources.get("pg_memory", "1g")),
        "ch_memory": str(resources.get("ch_memory", "2g")),
        "qdb_memory": str(resources.get("qdb_memory", "1g")),
        "hc_interval": str(healthcheck.get("interval", "2s")),
        "hc_retries": int(healthcheck.get("retries", 60)),
    }


# ---------------------------------------------------------------------------
# generators
# ---------------------------------------------------------------------------


def wipe_generated() -> None:
    for sub in (FLOWS_DIR, INIT_DIR):
        if sub.exists():
            shutil.rmtree(sub)
    for nested in ("source_pg", "sink_pg", "sink_ch", "sink_qdb", "state_pg"):
        (INIT_DIR / nested).mkdir(parents=True, exist_ok=True)
    FLOWS_DIR.mkdir(parents=True, exist_ok=True)


SOURCE_TABLE_TEMPLATE = """\
CREATE TABLE IF NOT EXISTS public.t_{nnn} (
    id          BIGINT PRIMARY KEY,
    user_id     BIGINT         NOT NULL,
    email       TEXT           NOT NULL,
    amount      NUMERIC(12, 2) NOT NULL,
    currency    TEXT           NOT NULL,
    status      TEXT           NOT NULL,
    description TEXT,
    quantity    INTEGER        NOT NULL,
    is_active   BOOLEAN        NOT NULL,
    metadata    JSONB,
    created_at  TIMESTAMPTZ    NOT NULL DEFAULT NOW(),
    updated_at  TIMESTAMPTZ    NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS t_{nnn}_cursor_idx ON public.t_{nnn} (updated_at, id);
"""

SINK_PG_TABLE_TEMPLATE = """\
CREATE TABLE IF NOT EXISTS public.{table_name} (
    id          BIGINT         PRIMARY KEY,
    user_id     BIGINT         NOT NULL,
    email       TEXT           NOT NULL,
    amount      NUMERIC(12, 2) NOT NULL,
    currency    TEXT           NOT NULL,
    status      TEXT           NOT NULL,
    description TEXT,
    quantity    INTEGER        NOT NULL,
    is_active   BOOLEAN        NOT NULL,
    metadata    JSONB,
    created_at  TIMESTAMPTZ    NOT NULL,
    updated_at  TIMESTAMPTZ    NOT NULL
);
"""

SINK_CH_TABLE_TEMPLATE = """\
CREATE TABLE IF NOT EXISTS {table_name} (
    id          Int64,
    user_id     Int64,
    email       String,
    amount      Decimal(12, 2),
    currency    String,
    status      LowCardinality(String),
    description Nullable(String),
    quantity    Int32,
    is_active   Bool,
    metadata    Nullable(String),
    created_at  DateTime('UTC'),
    updated_at  DateTime('UTC')
) ENGINE = {engine};
"""

# QuestDB has no `numeric(p,s)` — `amount` becomes DOUBLE (acceptable
# lossy for synthetic load). `email`/`currency`/`status` are SYMBOL for
# dictionary-encoded low-cardinality storage. `metadata` stays STRING
# (no JSON type in QuestDB on this image). `updated_at` is the
# designated TIMESTAMP, partitioned by day, WAL-enabled so DEDUP works.
SINK_QDB_TABLE_TEMPLATE = """\
CREATE TABLE IF NOT EXISTS {table_name} (
    id LONG,
    user_id LONG,
    email SYMBOL CAPACITY 1000,
    amount DOUBLE,
    currency SYMBOL CAPACITY 16,
    status SYMBOL CAPACITY 16,
    description STRING,
    quantity INT,
    is_active BOOLEAN,
    metadata STRING,
    created_at TIMESTAMP,
    updated_at TIMESTAMP
) TIMESTAMP(updated_at) PARTITION BY DAY{wal_clause}{dedup_clause};
"""


def write_source_sql(top: dict[str, Any]) -> None:
    for src_idx in range(top["src_count"]):
        body = "\n".join(
            SOURCE_TABLE_TEMPLATE.format(nnn=f"{tbl_idx:03d}")
            for tbl_idx in range(top["tables_per_source"])
        )
        out = INIT_DIR / "source_pg" / f"src{src_idx:02d}.sql"
        out.write_text(f"-- Generated by gen.py for source pg #{src_idx:02d}\n\n{body}", encoding="utf-8")


def sink_table_name(src_idx: int, tbl_idx: int) -> str:
    """Unique sink-side table name. Source-side stays plain ``t_NNN`` because
    each source pg is its own instance; sink instances are shared across
    sources, so the table name must encode the source index too."""
    return f"src_{src_idx:02d}_t_{tbl_idx:03d}"


def write_sink_pg_sql(top: dict[str, Any]) -> None:
    # Each sink pg only needs the (src, tbl) pairs routed to it.
    assigned: dict[int, list[tuple[int, int]]] = {i: [] for i in range(top["pg_count"])}
    for src_idx in range(top["src_count"]):
        for tbl_idx in range(top["tables_per_source"]):
            kind, slot = route(
                src_idx,
                tbl_idx,
                top["tables_per_source"],
                top["pg_count"],
                top["ch_count"],
                top["qdb_count"],
            )
            if kind == "postgres":
                assigned[slot].append((src_idx, tbl_idx))
    for slot, items in assigned.items():
        body = "\n".join(
            SINK_PG_TABLE_TEMPLATE.format(table_name=sink_table_name(s, t))
            for s, t in sorted(items)
        )
        out = INIT_DIR / "sink_pg" / f"snk{slot:02d}.sql"
        out.write_text(
            f"-- Generated by gen.py for sink pg #{slot:02d} — {len(items)} tables\n\n{body}",
            encoding="utf-8",
        )


def write_sink_ch_sql(top: dict[str, Any]) -> None:
    assigned: dict[int, list[tuple[int, int]]] = {i: [] for i in range(top["ch_count"])}
    for src_idx in range(top["src_count"]):
        for tbl_idx in range(top["tables_per_source"]):
            kind, slot = route(
                src_idx,
                tbl_idx,
                top["tables_per_source"],
                top["pg_count"],
                top["ch_count"],
                top["qdb_count"],
            )
            if kind == "clickhouse":
                assigned[slot].append((src_idx, tbl_idx))
    mutable_per_src = top["mutable_per_source"]
    for slot, items in assigned.items():
        parts: list[str] = []
        for s, t in sorted(items):
            engine = (
                "ReplacingMergeTree(updated_at) ORDER BY id"
                if is_mutable(t, mutable_per_src)
                else "MergeTree() ORDER BY (updated_at, id)"
            )
            parts.append(
                SINK_CH_TABLE_TEMPLATE.format(
                    table_name=sink_table_name(s, t),
                    engine=engine,
                )
            )
        body = "\n".join(parts)
        out = INIT_DIR / "sink_ch" / f"snk{slot:02d}.sql"
        out.write_text(
            f"-- Generated by gen.py for sink CH #{slot:02d} — {len(items)} tables\n\n{body}",
            encoding="utf-8",
        )


def write_sink_qdb_sql(top: dict[str, Any]) -> None:
    assigned: dict[int, list[tuple[int, int]]] = {i: [] for i in range(top["qdb_count"])}
    for src_idx in range(top["src_count"]):
        for tbl_idx in range(top["tables_per_source"]):
            kind, slot = route(
                src_idx,
                tbl_idx,
                top["tables_per_source"],
                top["pg_count"],
                top["ch_count"],
                top["qdb_count"],
            )
            if kind == "questdb":
                assigned[slot].append((src_idx, tbl_idx))
    mutable_per_src = top["mutable_per_source"]
    for slot, items in assigned.items():
        parts: list[str] = []
        for s, t in sorted(items):
            mutable = is_mutable(t, mutable_per_src)
            # DEDUP requires WAL; append-only tables use plain WAL too so
            # the engine path is uniform across all QuestDB tables here.
            wal_clause = " WAL"
            dedup_clause = (
                " DEDUP UPSERT KEYS(updated_at, id)" if mutable else ""
            )
            parts.append(
                SINK_QDB_TABLE_TEMPLATE.format(
                    table_name=sink_table_name(s, t),
                    wal_clause=wal_clause,
                    dedup_clause=dedup_clause,
                )
            )
        body = "\n".join(parts)
        out = INIT_DIR / "sink_qdb" / f"snk{slot:02d}.sql"
        out.write_text(
            f"-- Generated by gen.py for sink QDB #{slot:02d} — {len(items)} tables\n\n{body}",
            encoding="utf-8",
        )


def write_state_pg_sql() -> None:
    out = INIT_DIR / "state_pg" / "00-create-databases.sql"
    out.write_text(
        "-- Generated by gen.py — creates the airstate DB used by Air Elt's pg storage.\n"
        "CREATE DATABASE airstate;\n",
        encoding="utf-8",
    )


# ---------------------------------------------------------------------------
# docker-compose
# ---------------------------------------------------------------------------


def build_compose(top: dict[str, Any]) -> str:
    services: list[str] = []

    pg_mem = top["pg_memory"]
    ch_mem = top["ch_memory"]
    qdb_mem = top["qdb_memory"]
    hc_interval = top["hc_interval"]
    hc_retries = top["hc_retries"]

    def pg_service(name: str, host_port: int, init_file: str | None, memory: str) -> str:
        init_mount = ""
        if init_file:
            init_mount = (
                f"      - ./{init_file}:/docker-entrypoint-initdb.d/10-init.sql:ro\n"
            )
        # `max_connections=300` is headroom for air-elt's own per-sink pools
        # plus the load-gen / validator connections. `shared_buffers=256MB`
        # sized for the 1 GiB memory limit (postgres recommends 25%).
        return (
            f"  {name}:\n"
            f"    image: postgres:16\n"
            f"    command:\n"
            f"      - \"postgres\"\n"
            f"      - \"-c\"\n"
            f"      - \"max_connections=300\"\n"
            f"      - \"-c\"\n"
            f"      - \"shared_buffers=256MB\"\n"
            f"    environment:\n"
            f"      POSTGRES_USER: air\n"
            f"      POSTGRES_PASSWORD: air\n"
            f"      POSTGRES_DB: airdata\n"
            f"    ports:\n"
            f"      - \"{host_port}:5432\"\n"
            f"    volumes:\n"
            f"{init_mount}"
            f"    deploy:\n"
            f"      resources:\n"
            f"        limits:\n"
            f"          memory: {memory}\n"
            f"    healthcheck:\n"
            f"      test: [\"CMD-SHELL\", \"pg_isready -U air -d airdata\"]\n"
            f"      interval: {hc_interval}\n"
            f"      timeout: 3s\n"
            f"      retries: {hc_retries}\n"
        )

    def ch_service(name: str, http_port: int, tcp_port: int, init_file: str, memory: str) -> str:
        return (
            f"  {name}:\n"
            f"    image: clickhouse/clickhouse-server:24.3\n"
            f"    environment:\n"
            f"      CLICKHOUSE_USER: air\n"
            f"      CLICKHOUSE_PASSWORD: air\n"
            f"      CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT: \"1\"\n"
            f"    ulimits:\n"
            f"      nofile:\n"
            f"        soft: 262144\n"
            f"        hard: 262144\n"
            f"    ports:\n"
            f"      - \"{http_port}:8123\"\n"
            f"      - \"{tcp_port}:9000\"\n"
            f"    volumes:\n"
            f"      - ./{init_file}:/docker-entrypoint-initdb.d/10-init.sql:ro\n"
            f"    deploy:\n"
            f"      resources:\n"
            f"        limits:\n"
            f"          memory: {memory}\n"
            f"    healthcheck:\n"
            f"      test: [\"CMD-SHELL\", \"wget -q --spider http://localhost:8123/ping || exit 1\"]\n"
            f"      interval: {hc_interval}\n"
            f"      timeout: 3s\n"
            f"      retries: {hc_retries}\n"
        )

    def qdb_service(
        name: str, pg_wire_port: int, http_port: int, init_file: str, memory: str
    ) -> str:
        # QuestDB 9.4.3 is pinned to match the test handle's
        # `air_elt_commons_testing::questdb::QUESTDB_IMAGE_TAG` (native
        # 1-D DOUBLE[] arrays require >= 9.0). The old 8.1.1 pg-wire
        # mis-typing of extended-protocol bind parameters for any
        # non-STRING column (`inconvertible types: STRING -> BOOLEAN
        # [from=$N, to=<col>]` at validate-time) was fixed back in 8.2.3,
        # so the sink's dry-run probe — and every typed INSERT — works.
        # PG-wire credentials match the test's `air:air` convention so the
        # same URL scheme works everywhere. (No tmpfs mount — earlier
        # iterations used one but memory pressure made it unviable at 18+
        # containers; QuestDB now writes to the container's own ephemeral
        # writable layer, which `compose down -v` reclaims.)
        return (
            f"  {name}:\n"
            f"    image: questdb/questdb:9.4.3\n"
            f"    environment:\n"
            f"      QDB_PG_USER: air\n"
            f"      QDB_PG_PASSWORD: air\n"
            f"      QDB_TELEMETRY_ENABLED: \"false\"\n"
            f"    ports:\n"
            f"      - \"{pg_wire_port}:8812\"\n"
            f"      - \"{http_port}:9000\"\n"
            f"    volumes:\n"
            # QuestDB has no auto-init hook like postgres/clickhouse;
            # the init SQL is mounted read-only for out-of-band apply
            # (operator or follow-up run.py step) over pg-wire.
            f"      - ./{init_file}:/etc/questdb-init.sql:ro\n"
            f"    deploy:\n"
            f"      resources:\n"
            f"        limits:\n"
            f"          memory: {memory}\n"
            f"    healthcheck:\n"
            f"      test: [\"CMD\", \"bash\", \"-c\", \"exec 3<>/dev/tcp/127.0.0.1/9000 || exit 1\"]\n"
            f"      interval: {hc_interval}\n"
            f"      timeout: 3s\n"
            f"      retries: {hc_retries}\n"
        )

    for i in range(top["src_count"]):
        services.append(
            pg_service(
                f"src_pg_{i:02d}",
                top["src_pg_base"] + i,
                f"init/source_pg/src{i:02d}.sql",
                pg_mem,
            )
        )
    for i in range(top["pg_count"]):
        services.append(
            pg_service(
                f"sink_pg_{i:02d}",
                top["sink_pg_base"] + i,
                f"init/sink_pg/snk{i:02d}.sql",
                pg_mem,
            )
        )
    for i in range(top["ch_count"]):
        services.append(
            ch_service(
                f"sink_ch_{i:02d}",
                top["sink_ch_http_base"] + i,
                top["sink_ch_tcp_base"] + i,
                f"init/sink_ch/snk{i:02d}.sql",
                ch_mem,
            )
        )
    for i in range(top["qdb_count"]):
        services.append(
            qdb_service(
                f"sink_qdb_{i:02d}",
                top["questdb_pg_base"] + i,
                top["questdb_http_base"] + i,
                f"init/sink_qdb/snk{i:02d}.sql",
                qdb_mem,
            )
        )
    services.append(
        pg_service(
            "state_pg",
            top["state_pg"],
            "init/state_pg/00-create-databases.sql",
            pg_mem,
        )
    )

    return (
        f"# Generated by gen.py — DO NOT EDIT.\n"
        f"name: {COMPOSE_PROJECT}\n\n"
        f"services:\n"
        + "\n".join(services)
    )


# ---------------------------------------------------------------------------
# air-elt config + flows
# ---------------------------------------------------------------------------


def build_config_toml(top: dict[str, Any]) -> str:
    lines: list[str] = []
    lines.append("# Generated by gen.py — DO NOT EDIT.")
    lines.append("[config]")
    lines.append('include = ["flows"]')
    lines.append("")
    lines.append("[secrets]")
    for i in range(top["src_count"]):
        port = top["src_pg_base"] + i
        lines.append(f'SRC_PG_{i:02d}_URL = "postgres://air:air@localhost:{port}/airdata"')
    for i in range(top["pg_count"]):
        port = top["sink_pg_base"] + i
        lines.append(f'SINK_PG_{i:02d}_URL = "postgres://air:air@localhost:{port}/airdata"')
    for i in range(top["ch_count"]):
        port = top["sink_ch_http_base"] + i
        lines.append(f'SINK_CH_{i:02d}_URL = "http://localhost:{port}"')
    for i in range(top["qdb_count"]):
        port = top["questdb_pg_base"] + i
        lines.append(
            f'SINK_QDB_{i:02d}_URL = "postgres://air:air@localhost:{port}/qdb"'
        )
    lines.append(
        f'STATE_PG_URL  = "postgres://air:air@localhost:{top["state_pg"]}/airstate"'
    )
    lines.append("")

    for i in range(top["src_count"]):
        lines.append("[[sources]]")
        lines.append(f'name = "src_pg_{i:02d}"')
        lines.append('type = "postgres"')
        lines.append(
            f'config = {{ url = "${{SRC_PG_{i:02d}_URL}}", max-connections = 20 }}'
        )
        lines.append("")

    for i in range(top["pg_count"]):
        lines.append("[[sinks]]")
        lines.append(f'name = "sink_pg_{i:02d}"')
        lines.append('type = "postgres"')
        lines.append(
            f'config = {{ url = "${{SINK_PG_{i:02d}_URL}}", max-connections = 20 }}'
        )
        lines.append("")
    for i in range(top["ch_count"]):
        lines.append("[[sinks]]")
        lines.append(f'name = "sink_ch_{i:02d}"')
        lines.append('type = "clickhouse"')
        lines.append(
            f'config = {{ url = "${{SINK_CH_{i:02d}_URL}}", database = "default", '
            f'user = "air", password = "air", max-connections = 20 }}'
        )
        lines.append("")
    for i in range(top["qdb_count"]):
        lines.append("[[sinks]]")
        lines.append(f'name = "sink_qdb_{i:02d}"')
        lines.append('type = "questdb"')
        # The QuestDB sink uses the pg-wire protocol; credentials and
        # database name are embedded in the URL. The sink config struct
        # accepts only url + pool tunables (deny_unknown_fields).
        lines.append(
            f'config = {{ url = "${{SINK_QDB_{i:02d}_URL}}", max-connections = 20 }}'
        )
        lines.append("")

    lines.append("[[storages]]")
    lines.append('name = "pg_state"')
    lines.append('type = "postgres"')
    lines.append('config = { url = "${STATE_PG_URL}", max-connections = 16 }')
    lines.append("")
    return "\n".join(lines)


FLOW_TEMPLATE = """\
# Generated by gen.py — DO NOT EDIT.
[flow.{flow_name}]
source  = "{source_name}"
sink    = "{sink_name}"
storage = "pg_state"

from = "public.t_{tbl_nnn}"
to   = "{to_target}"

batch-limit   = {batch_limit}
query-timeout = "{query_timeout}"

[flow.{flow_name}.mapping]
id          = "id"
user_id     = "user_id"
email       = "email"
amount      = "amount"
currency    = "currency"
status      = "status"
description = "description"
quantity    = "quantity"
is_active   = "is_active"
metadata    = "metadata"
created_at  = "created_at"
updated_at  = "updated_at"

[flow.{flow_name}.cursor]
fields   = ["updated_at", "id"]
order    = "asc"
interval = "{cursor_interval}"{cursor_jitter_line}

[flow.{flow_name}.validation]
sampling = false
"""

# Appended verbatim for mutable → postgres flows. Mutable CH flows rely on
# the ReplacingMergeTree engine instead — the CH sink is append-only at the
# Air Elt runtime layer and dedups at merge time.
PG_CONFLICT_BLOCK = """
[flow.{flow_name}.conflict]
key      = ["id"]
strategy = "overwrite"
"""


def write_flows(top: dict[str, Any]) -> int:
    written = 0
    mutable_per_src = top["mutable_per_source"]
    for src_idx in range(top["src_count"]):
        for tbl_idx in range(top["tables_per_source"]):
            kind, slot = route(
                src_idx,
                tbl_idx,
                top["tables_per_source"],
                top["pg_count"],
                top["ch_count"],
                top["qdb_count"],
            )
            table_name = sink_table_name(src_idx, tbl_idx)
            if kind == "postgres":
                sink_name = f"sink_pg_{slot:02d}"
                to_target = f"public.{table_name}"
            elif kind == "clickhouse":
                sink_name = f"sink_ch_{slot:02d}"
                to_target = table_name
            else:
                sink_name = f"sink_qdb_{slot:02d}"
                to_target = table_name
            flow_name = f"src{src_idx:02d}_tbl{tbl_idx:03d}"
            jitter = top["cursor_jitter"]
            jitter_line = f'\njitter   = "{jitter}"' if jitter else ""
            body = FLOW_TEMPLATE.format(
                flow_name=flow_name,
                source_name=f"src_pg_{src_idx:02d}",
                sink_name=sink_name,
                tbl_nnn=f"{tbl_idx:03d}",
                to_target=to_target,
                batch_limit=top["batch_limit"],
                query_timeout=top["query_timeout"],
                cursor_interval=top["cursor_interval"],
                cursor_jitter_line=jitter_line,
            )
            if kind == "postgres" and is_mutable(tbl_idx, mutable_per_src):
                body += PG_CONFLICT_BLOCK.format(flow_name=flow_name)
            (FLOWS_DIR / f"{flow_name}.toml").write_text(body, encoding="utf-8")
            written += 1
    return written


# ---------------------------------------------------------------------------
# .env.generated
# ---------------------------------------------------------------------------


def build_env(top: dict[str, Any]) -> str:
    lines: list[str] = ["# Generated by gen.py — sourced by run.py."]
    for i in range(top["src_count"]):
        port = top["src_pg_base"] + i
        lines.append(
            f'export SRC_PG_{i:02d}_URL="postgres://air:air@localhost:{port}/airdata"'
        )
    for i in range(top["pg_count"]):
        port = top["sink_pg_base"] + i
        lines.append(
            f'export SINK_PG_{i:02d}_URL="postgres://air:air@localhost:{port}/airdata"'
        )
    for i in range(top["ch_count"]):
        port = top["sink_ch_http_base"] + i
        lines.append(f'export SINK_CH_{i:02d}_URL="http://localhost:{port}"')
    # ClickHouse credentials — sink config bakes in `air:air`, validate.py
    # reads these to forward HTTP Basic auth headers.
    lines.append('export CLICKHOUSE_USER="air"')
    lines.append('export CLICKHOUSE_PASSWORD="air"')
    for i in range(top["qdb_count"]):
        port = top["questdb_pg_base"] + i
        lines.append(
            f'export SINK_QDB_{i:02d}_URL="postgres://air:air@localhost:{port}/qdb"'
        )
    lines.append(
        f'export STATE_PG_URL="postgres://air:air@localhost:{top["state_pg"]}/airstate"'
    )
    lines.append("")
    return "\n".join(lines)


# ---------------------------------------------------------------------------
# entrypoint
# ---------------------------------------------------------------------------


def print_routing(top: dict[str, Any]) -> None:
    print(f"# Routing for {top['src_count']}x{top['tables_per_source']} flows")
    print("# flow_name\tsink\theavy/light\tmutable/append")
    for src_idx in range(top["src_count"]):
        for tbl_idx in range(top["tables_per_source"]):
            kind, slot = route(
                src_idx,
                tbl_idx,
                top["tables_per_source"],
                top["pg_count"],
                top["ch_count"],
                top["qdb_count"],
            )
            if kind == "postgres":
                sink = f"sink_pg_{slot:02d}"
            elif kind == "clickhouse":
                sink = f"sink_ch_{slot:02d}"
            else:
                sink = f"sink_qdb_{slot:02d}"
            heavy = "heavy" if is_heavy(tbl_idx, top["heavy_per_source"]) else "light"
            mut = "mutable" if is_mutable(tbl_idx, top["mutable_per_source"]) else "append"
            print(f"src{src_idx:02d}_tbl{tbl_idx:03d}\t{sink}\t{heavy}\t{mut}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--print-routing",
        action="store_true",
        help="Print the (flow -> sink, heavy/light) routing table and exit without writing files.",
    )
    args = parser.parse_args()

    top = load_topology(TOPOLOGY_PATH)
    if args.print_routing:
        print_routing(top)
        return 0

    wipe_generated()
    write_source_sql(top)
    write_sink_pg_sql(top)
    write_sink_ch_sql(top)
    write_sink_qdb_sql(top)
    write_state_pg_sql()

    COMPOSE_PATH.write_text(build_compose(top), encoding="utf-8")
    CONFIG_PATH.parent.mkdir(parents=True, exist_ok=True)
    CONFIG_PATH.write_text(build_config_toml(top), encoding="utf-8")
    flows_written = write_flows(top)
    ENV_PATH.write_text(build_env(top), encoding="utf-8")

    total_containers = (
        top["src_count"] + top["pg_count"] + top["ch_count"] + top["qdb_count"] + 1
    )
    print(
        f"[gen.py] wrote {flows_written} flow .toml files, "
        f"{total_containers} container definitions, config.toml, init SQL, .env.generated."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
