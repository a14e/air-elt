//! SQL emitted by the storage crate.
//!
//! Why no schema plumbing: `PgStorageConfig` deliberately has no `schema`
//! field — the cursor table lives in whatever search_path the pool session
//! provides (default: `$user, public`). Operators who need a different
//! schema encode it in the URL via `?options=-c%20search_path%3D<schema>`,
//! which libpq applies on every new pool connection so migrations and
//! runtime queries agree without us touching `SET search_path` ourselves.

pub const CURSORS_TABLE: &str = "air_elt_cursors";

pub const PING: &str = "SELECT 1";

pub const TABLE_EXISTS: &str = "SELECT EXISTS (
    SELECT 1 FROM information_schema.tables
    WHERE table_name = $1
      AND table_schema = current_schema()
)";

pub const HAS_CREATE_PRIVILEGE: &str =
    "SELECT has_schema_privilege(current_user, current_schema(), 'CREATE') AS ok";

pub const PROBE_INSERT_WHERE_FALSE: &str = "INSERT INTO air_elt_cursors (flow, state) \
    SELECT flow, state FROM air_elt_cursors WHERE false";

pub const SELECT_CURSOR: &str = "SELECT state FROM air_elt_cursors WHERE flow = $1";

pub const UPSERT_CURSOR: &str = "INSERT INTO air_elt_cursors (flow, state, updated_at) \
    VALUES ($1, $2, now()) \
    ON CONFLICT (flow) DO UPDATE SET state = EXCLUDED.state, updated_at = now()";
