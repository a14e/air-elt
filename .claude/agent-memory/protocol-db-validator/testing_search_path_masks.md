---
name: Test helper masks search_path bugs
description: PgTestHandle::url_with_search_path embeds libpq options=-c search_path in the URL, so every pool connection gets search_path set — which hides per-connection search_path bugs in production code
type: project
---

`crates/commons/src/testing/pg.rs` builds URLs of the form
`postgres://.../db?options=-c%20search_path%3D<schema>`. libpq applies those
options at session start for every new connection, so any pool built on
that URL has effectively session-sticky search_path.

Production `PgStorageConfig.url` typically does NOT carry those options,
so any code path that relies on "SET search_path" carrying across pool
connections will pass tests and fail in prod.

**Why:** Tests that green-light a connection-global session setting by
hard-coding it into the URL will silently hide bugs in the session-management
code of the crate under test.

**How to apply:** When auditing any sqlx code that touches session-scope
configuration (SET, SET TIME ZONE, SET ROLE, SET application_name, current
schema), trace whether:
(a) the test URL embeds libpq `options=-c ...`, AND
(b) the prod config path lets the user provide a bare URL without those options.
If both hold, the test is a poor oracle for the prod behaviour. Prefer
`PgPoolOptions::after_connect` for session config so both paths agree.

Tests that rely on this pattern today:
- `crates/app/tests/pg_to_pg.rs:73,90`
- `crates/sources/postgres/tests/source_e2e.rs:34`
- `crates/sinks/postgres/tests/sink_e2e.rs:27`
- `crates/storages/postgres/tests/storage_e2e.rs:10`
