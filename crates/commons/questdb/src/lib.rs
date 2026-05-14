//! Shared QuestDB helpers for the sink and (future) source connectors.
//!
//! Structural twin of `air-elt-commons-clickhouse` but for QuestDB. The
//! crate owns:
//!
//! * pg-wire pool helpers ([`pool`]) — free functions building a
//!   `sqlx::PgPool` for the control plane and the production INSERT
//!   path, plus a `SELECT 1` ping,
//! * identifier quoting ([`identifier`]) — Postgres-style `"…"`,
//! * schema introspection via `SHOW COLUMNS FROM "<table>"` ([`schema`])
//!   and the QuestDB native type-string parser ([`qd_type_parser`]),
//! * the pg-wire `Separated` binding helper ([`pg_bind`]) — the only
//!   place that translates canonical [`air_elt_core::types::Value`] into
//!   sqlx bind calls,
//! * the QuestDB-specific `DynType` / `DynValue` registry under
//!   [`types`] (`SYMBOL`, `LONG256`, `IPv4`, `GEOHASH`).
//!
//! This crate must NOT depend on `air-elt-commons-pg`. The Postgres
//! connector crates are isolated; QuestDB's pg-wire support is built on
//! plain `sqlx::PgPool` so the two backends never share connectors.

pub mod identifier;
pub mod pg_bind;
pub mod pool;
pub mod qd_type_parser;
pub mod schema;
pub mod types;

pub use qd_type_parser::{ParseError, parse_type};
pub use schema::{SchemaError, SchemaWithDesignated, fetch_schema};
