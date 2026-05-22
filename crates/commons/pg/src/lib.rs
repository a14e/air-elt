//! PostgreSQL-specific SQL helpers shared by all pg connectors.
//!
//! The db-agnostic counterparts (identifier validation, pool timeouts, value
//! conversions) live in `air-elt-commons`. This crate is the dialect-aware
//! layer: pg quoting, pg pool construction, pg type table, pg null binding.

pub mod dialect;
pub mod identifier;
pub mod null_bind;
pub mod pg_type;
pub mod pool;
pub mod pool_stats_reader;
pub mod retry;
pub mod schema;
pub mod sink_bind;
pub mod types;

pub use dialect::Dialect;
pub use pool_stats_reader::PgPoolStatsReader;
pub use types::{PgHllType, PgHllValue};
