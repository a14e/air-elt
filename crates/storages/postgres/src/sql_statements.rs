//! SQL emitted by the storage crate. Two state tables live here:
//! `air_elt_cursors` for column-cursor state (pull-based sources) and
//! `air_elt_resume_tokens` for opaque CDC resume tokens (mongo-cdc) —
//! see `Storage::{load,save}_resume_token`. Both share the same
//! search-path placement.
//!
//! Why no schema plumbing: `PgStorageConfig` deliberately has no `schema`
//! field — both state tables live in whatever search_path the pool
//! session provides (default: `$user, public`). Operators who need a
//! different schema encode it in the URL via
//! `?options=-c%20search_path%3D<schema>`, which libpq applies on every
//! new pool connection so migrations and runtime queries agree without
//! us touching `SET search_path` ourselves.

pub const CURSORS_TABLE: &str = "air_elt_cursors";

pub const PING: &str = "SELECT 1";

pub const TABLE_EXISTS: &str = "SELECT EXISTS (
    SELECT 1 FROM information_schema.tables
    WHERE table_name = $1
      AND table_schema = current_schema()
)";

pub const HAS_CREATE_PRIVILEGE: &str =
    "SELECT has_schema_privilege(current_user, current_schema(), 'CREATE') AS ok";

pub const HAS_TABLE_INSERT: &str = "SELECT has_table_privilege(current_user, current_schema() || '.air_elt_cursors', 'INSERT') AS ok";

pub const PROBE_INSERT_WHERE_FALSE: &str = "INSERT INTO air_elt_cursors (flow, state) \
    SELECT flow, state FROM air_elt_cursors WHERE false";

pub const SELECT_CURSOR: &str = "SELECT state FROM air_elt_cursors WHERE flow = $1";

pub const UPSERT_CURSOR: &str = "INSERT INTO air_elt_cursors (flow, state, updated_at) \
    VALUES ($1, $2, now()) \
    ON CONFLICT (flow) DO UPDATE SET state = EXCLUDED.state, updated_at = now()";

pub const SELECT_RESUME_TOKEN: &str = "SELECT token FROM air_elt_resume_tokens WHERE flow = $1";

pub const UPSERT_RESUME_TOKEN: &str = "INSERT INTO air_elt_resume_tokens (flow, token, updated_at) \
    VALUES ($1, $2, now()) \
    ON CONFLICT (flow) DO UPDATE SET token = EXCLUDED.token, updated_at = now()";
