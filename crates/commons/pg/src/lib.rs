//! PostgreSQL-specific SQL helpers shared by all pg connectors.
//!
//! The db-agnostic counterparts (identifier validation, pool timeouts, value
//! conversions) live in `air-elt-commons`. This crate is the dialect-aware
//! layer: pg quoting, pg pool construction, pg type table, pg null binding.

pub mod identifier;
pub mod null_bind;
pub mod pg_type;
pub mod pool;
pub mod schema;
